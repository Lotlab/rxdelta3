//! Merge orchestration: single-file merge + directory merge.

use std::cell::Cell;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

/// `CreateProcess` flag: run the child without allocating/inheriting a console,
/// so spawning the console-subsystem `xdelta.exe` from the GUI host does not
/// flash a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

use rxdelta::checksum::Checksum as _;
use rxdelta::{ApplyOptions, ApplyOutcome, ChecksumAlgo};

use crate::dat::{self, PatchDat};
use crate::logln;
use crate::msgs;
use crate::win32;

struct Stopwatch(std::time::Instant);

impl Stopwatch {
    fn start() -> Self {
        Stopwatch(std::time::Instant::now())
    }
    fn ms(&self) -> f64 {
        self.0.elapsed().as_secs_f64() * 1000.0
    }
}

// ---------------------------------------------------------------------------
// Callback types (__stdcall)
// ---------------------------------------------------------------------------

pub type FileProgressCb = unsafe extern "stdcall" fn(msg: *const u16, ty: i32);
pub type CmdProgressCb = unsafe extern "stdcall" fn(done: i32, total: i32);
pub type MergeCb = unsafe extern "stdcall" fn(src: *const u16, delta: *const u16, out: *const u16) -> i32;

// ---------------------------------------------------------------------------
// Wide-string / path helpers
// ---------------------------------------------------------------------------

/// Convert a NUL-terminated UTF-16 pointer into an owned Rust PathBuf.
unsafe fn u16_to_path(p: *const u16) -> Option<PathBuf> {
    if p.is_null() {
        return None;
    }
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    if len == 0 {
        return None;
    }
    let slice = std::slice::from_raw_parts(p, len);
    Some(PathBuf::from(std::ffi::OsString::from_wide(slice)))
}

fn path_to_wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn join(src: &Path, name: &str) -> PathBuf {
    src.join(name)
}

/// Case-insensitive path equality on Windows (approximates `__wcsicmp`).
fn paths_equal(a: &Path, b: &Path) -> bool {
    let aw: Vec<u16> = a.as_os_str().encode_wide().collect();
    let bw: Vec<u16> = b.as_os_str().encode_wide().collect();
    if aw.len() != bw.len() {
        return false;
    }
    aw.iter().zip(bw.iter()).all(|(x, y)| {
        let cx = char::from_u32(*x as u32).unwrap_or('\u{FFFD}');
        let cy = char::from_u32(*y as u32).unwrap_or('\u{FFFD}');
        cx.eq_ignore_ascii_case(&cy)
    })
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

fn file_size(p: &Path) -> i64 {
    fs::metadata(p).map(|m| m.len() as i64).unwrap_or(0)
}

fn file_exists(p: &Path) -> bool {
    fs::metadata(p).is_ok()
}

/// Create all parent directories of `p`.
fn ensure_directory_path(p: &Path) -> bool {
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            return fs::create_dir_all(parent).is_ok();
        }
    }
    true
}

/// Clear the read-only attribute if present (best-effort). On Windows
/// `Permissions::readonly` maps to the FILE_ATTRIBUTE_READONLY attribute, so
/// no Win32 FFI is needed.
fn clear_read_only(p: &Path) {
    if let Ok(meta) = std::fs::metadata(p) {
        let mut perms = meta.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(p, perms);
        }
    }
}

/// Read the delta and return its exact target output length in bytes, via the
/// cheap header-only scan (no decompression). Returns None when the delta is
/// unreadable or malformed. Uses mmap so a large delta isn't copied into the
/// 32-bit process's address space.
fn delta_target_len(delta: &Path) -> Option<i64> {
    let sw = Stopwatch::start();
    let map = match rxdelta::io::MappedFile::open(delta) {
        Ok(m) => m,
        Err(e) => {
            logln!(
                "[timing] delta_target_len mmap FAILED {}: {} ({:.1}ms)",
                delta.display(),
                e,
                sw.ms()
            );
            return None;
        }
    };
    let r = rxdelta::decoder::delta_target_len(map.as_bytes(), rxdelta::DEFAULT_MAX_WINDOW)
        .ok()
        .map(|n| n as i64);
    logln!(
        "[timing] delta_target_len {} = {:?} ({:.1}ms)",
        delta.display(),
        r,
        sw.ms()
    );
    r
}

/// Required free bytes to apply `delta` against `src`.
/// A true in-place rewrite (src == dst, only changed slots written) needs only
/// the file growth beyond the current source size; a full write needs the
/// exact target length. Falls back to the conservative `src + delta` estimate
/// when the delta can't be parsed.
fn required_apply_bytes(src: &Path, delta: &Path, in_place: bool) -> i64 {
    let sw = Stopwatch::start();
    let src_size = file_size(src);
    let required = if in_place {
        match delta_target_len(delta) {
            Some(target) => (target - src_size).max(0),
            None => src_size + file_size(delta),
        }
    } else {
        match delta_target_len(delta) {
            Some(target) => target,
            None => src_size + file_size(delta),
        }
    };
    logln!(
        "[timing] required_apply_bytes in_place={} src={:.1}MB delta={:.1}MB required={:.1}MB ({:.1}ms)",
        in_place,
        src_size as f64 / 1048576.0,
        file_size(delta) as f64 / 1048576.0,
        required as f64 / 1048576.0,
        sw.ms()
    );
    required
}

/// Check free space on the volume holding `p` against `required` bytes.
/// Mirrors the original `GetDiskFreeSpaceExW`-based check (returns false when
/// insufficient). The original showed a dialog; we simply report failure.
fn check_disk_space(p: &Path, required: i64) -> bool {
    let sw = Stopwatch::start();
    // GetDiskFreeSpaceExW needs a path whose volume can be resolved; use the
    // parent directory (or the path itself if it is already a directory root).
    let target = p
        .parent()
        .filter(|pp| !pp.as_os_str().is_empty())
        .unwrap_or(p);
    let wide = path_to_wide(target);
    let mut free: win32::ULARGE_INTEGER = win32::ULARGE_INTEGER { low: 0, high: 0 };
    let ok = unsafe {
        win32::GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut())
    };
    if ok == 0 {
        // Cannot determine; original treats failure as "not enough".
        logln!(
            "[timing] check_disk_space FAILED to query {} ({:.1}ms)",
            target.display(),
            sw.ms()
        );
        return false;
    }
    let free_bytes = free.quad_part() as i64;
    let enough = free_bytes >= required;
    logln!(
        "[timing] check_disk_space {} need={:.1}MB free={:.1}MB ok={} ({:.1}ms)",
        target.display(),
        required as f64 / 1048576.0,
        free_bytes as f64 / 1048576.0,
        enough,
        sw.ms()
    );
    enough
}

/// Recursively copy `src` into `dst` (aligns with `xcopy /e /y`: includes
/// empty directories, overwrites existing files). Files whose path relative
/// to `src` is in `skip` are not copied (they are produced by patching).
/// Does not follow symlinks.
fn copy_tree(src: &Path, dst: &Path, skip: Option<&std::collections::HashSet<PathBuf>>) -> bool {
    copy_tree_inner(src, src, dst, skip, true)
}

fn copy_tree_inner(
    root_src: &Path,
    src: &Path,
    dst: &Path,
    skip: Option<&std::collections::HashSet<PathBuf>>,
    is_root: bool,
) -> bool {
    let sw = Stopwatch::start();
    if !dst.exists() {
        if fs::create_dir_all(dst).is_err() {
            return false;
        }
    }
    let read_dir = match fs::read_dir(src) {
        Ok(rd) => rd,
        Err(_) => return false,
    };
    for entry in read_dir {
        let Ok(entry) = entry else { return false };
        let src_child = entry.path();
        let dst_child = dst.join(entry.file_name());
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => return false,
        };
        if ft.is_dir() {
            if !copy_tree_inner(root_src, &src_child, &dst_child, skip, false) {
                return false;
            }
        } else if ft.is_file() {
            // Skip files that will be produced by patching.
            if let Some(set) = skip {
                if let Ok(rel) = src_child.strip_prefix(root_src) {
                    if set.contains(rel) {
                        logln!("[copy-skip] {}", rel.display());
                        continue;
                    }
                }
            }
            let fsw = Stopwatch::start();
            if !ensure_directory_path(&dst_child) {
                return false;
            }
            if fs::copy(&src_child, &dst_child).is_err() {
                return false;
            }
            logln!(
                "copy {:.1}MB +{:.0}ms",
                file_size(&src_child) as f64 / 1048576.0,
                fsw.ms()
            );
        }
    }
    if is_root {
        logln!("copy_tree done in {:.0}ms", sw.ms());
    }
    true
}

/// Copy a single file with readonly-clear + directory creation.
fn copy_file(src: &Path, dst: &Path) -> bool {
    let sw = Stopwatch::start();
    let ok = if !ensure_directory_path(dst) {
        false
    } else {
        clear_read_only(dst);
        fs::copy(src, dst).is_ok()
    };
    logln!(
        "[timing] copy_file {} ({:.1}MB) ok={} ({:.1}ms)",
        dst.display(),
        file_size(src) as f64 / 1048576.0,
        ok,
        sw.ms()
    );
    ok
}

/// Delete a file, clearing read-only first.
fn delete_file(p: &Path) -> bool {
    clear_read_only(p);
    match fs::remove_file(p) {
        Ok(_) => true,
        Err(_) => false,
    }
}

/// Compute MD5 hex (uppercase, matching the dat sample) of a file. Returns
/// None when the file is missing/unreadable.
fn file_md5_upper(p: &Path) -> Option<String> {
    let sw = Stopwatch::start();
    let size = file_size(p);
    let result = (|| -> Option<String> {
        let mut h = rxdelta::checksum::Md5::new();
        let f = File::open(p).ok()?;
        let mut reader = std::io::BufReader::new(f);
        let mut buf = [0u8; 65536];
        loop {
            let n = reader.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Some(rxdelta::encode_hex(&h.digest()).to_uppercase())
    })();
    logln!(
        "[timing] md5 {} ({:.1}MB) ok={} ({:.1}ms)",
        p.display(),
        size as f64 / 1048576.0,
        result.is_some(),
        sw.ms()
    );
    result
}

/// Case-insensitive hex digest comparison (dat uses uppercase, xdelta lower).
fn md5_hex_eq_ci(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

// ---------------------------------------------------------------------------
// Single-file merge (MergeFile internal)
// ---------------------------------------------------------------------------

const APPLY_OPTS: ApplyOptions = ApplyOptions {
    verify_checksums: true,
    max_window_size: rxdelta::DEFAULT_MAX_WINDOW,
    idempotent_skip: true,
};

/// Decode `delta` against `src` writing to `dst`, following `MergeFileInternal`.
/// - If `src == dst` (case-insensitive): apply in place, rewriting only the
///   changed window slots (pure-copy windows are skipped). Falls back to the
///   tmp+rename path when an external `merge_cb` is supplied (it can't write
///   in place) or the delta is mostly changed.
/// - `expected_md5` enables idempotent skip + post-verify (dir path only).
fn merge_file_internal(
    src: &Path,
    delta: &Path,
    dst: &Path,
    merge_cb: Option<MergeCb>,
    expected_md5: Option<&str>,
) -> bool {
    let in_place = paths_equal(src, dst);
    // True in-place apply (only changed slots written) needs an internal
    // callback; an external callback falls back to the tmp+rename full write.
    let use_internal = match merge_cb {
        None => true,
        Some(cb) => cb as *const () == self_merge_file_ptr(),
    };

    // Disk space pre-check: a true in-place rewrite needs only the growth
    // beyond the current file; a full write needs the exact target size.
    let required = required_apply_bytes(src, delta, in_place && use_internal);
    if required > 0 && !check_disk_space(dst, required) {
        logln!(
            "[error] disk space check failed: need {} bytes at {}",
            required,
            dst.display()
        );
        return false;
    }

    if in_place {
        if use_internal {
            clear_read_only(src);
            let sw = Stopwatch::start();
            let ok = in_place_apply(src, delta, expected_md5);
            logln!(
                "[in-place] {} -> {} in {:.0}ms",
                src.display(),
                if ok { "ok" } else { "FAIL" },
                sw.ms()
            );
            return ok;
        }
    }

    let actual_dst = if in_place {
        PathBuf::from(format!("{}.tmp", dst.display()))
    } else {
        dst.to_path_buf()
    };

    // Ensure destination parent exists.
    if !ensure_directory_path(&actual_dst) {
        logln!(
            "[error] cannot create destination directory: {}",
            actual_dst.display()
        );
        return false;
    }
    clear_read_only(&actual_dst);

    let result = apply_single(
        src,
        delta,
        &actual_dst,
        merge_cb,
        expected_md5,
    );

    if in_place {
        if result {
            // Delete src, move tmp → src.
            if delete_file(src) && fs::rename(&actual_dst, src).is_ok() {
                true
            } else {
                logln!("[error] in-place replace failed: cannot move tmp to {}", src.display());
                let _ = fs::remove_file(&actual_dst);
                false
            }
        } else {
            let _ = fs::remove_file(&actual_dst);
            false
        }
    } else {
        result
    }
}

/// Apply a delta in place, writing only changed slots, then optionally verify
/// the resulting file's MD5 against `expected_md5`.
fn in_place_apply(src: &Path, delta: &Path, expected_md5: Option<&str>) -> bool {
    let src_size = file_size(src);
    // Large source: 32-bit cannot mmap; delegate to the 64-bit subprocess.
    if src_size > LARGE_FILE_THRESHOLD as i64 {
        logln!(
            "[timing] in_place_apply mode=subprocess src={:.1}MB",
            src_size as f64 / 1048576.0
        );
        return subprocess_decode_in_place(src, delta, expected_md5);
    }
    let asw = Stopwatch::start();
    // With an expected MD5, verify inside the library: the output hash is
    // checked before the source is touched, so a mismatch fails with the
    // file still intact; write-phase I/O failures roll back via the journal.
    // This also replaces the post-apply full-file MD5 re-read.
    if let Some(md5) = expected_md5 {
        let expect_after = match rxdelta::checksum::decode_hex(md5) {
            Some(b) => b,
            None => {
                logln!("[error] in-place apply failed: invalid expected md5: {md5}");
                return false;
            }
        };
        let outcome = rxdelta::apply_paths_in_place_verified(
            src,
            delta,
            &APPLY_OPTS,
            rxdelta::ChecksumAlgo::Md5,
            None,
            Some(&expect_after),
        );
        logln!(
            "[timing] in_place_apply core src={:.1}MB ({:.1}ms)",
            src_size as f64 / 1048576.0,
            asw.ms()
        );
        return match outcome {
            Ok(rxdelta::InPlaceOutcome::Applied { stats, .. }) => {
                logln!(
                    "[in-place] windows={} written={} ({} bytes) skipped={} verified",
                    stats.windows,
                    stats.written_windows,
                    stats.written_bytes,
                    stats.skipped_windows
                );
                true
            }
            Ok(rxdelta::InPlaceOutcome::Skipped { .. }) => {
                logln!("[in-place] skipped: already patched");
                true
            }
            Err(e) => {
                logln!("[error] in-place apply failed after {:.1}ms: {e}", asw.ms());
                false
            }
        };
    }
    let stats = match rxdelta::apply_paths_in_place(src, delta, &APPLY_OPTS) {
        Ok(s) => s,
        Err(e) => {
            logln!("[error] in-place apply failed after {:.1}ms: {e}", asw.ms());
            return false;
        }
    };
    logln!(
        "[timing] in_place_apply core src={:.1}MB ({:.1}ms)",
        src_size as f64 / 1048576.0,
        asw.ms()
    );
    logln!(
        "[in-place] windows={} written={} ({} bytes) skipped={}",
        stats.windows,
        stats.written_windows,
        stats.written_bytes,
        stats.skipped_windows
    );
    true
}

/// Decode one file: internal path via rxdelta, or external `merge_cb`.
fn apply_single(
    src: &Path,
    delta: &Path,
    dst: &Path,
    merge_cb: Option<MergeCb>,
    expected_md5: Option<&str>,
) -> bool {
    // Self-reference detection: if the caller passed our own MergeFile export,
    // route to the internal (inline-hash) path — no output read-back.
    let use_internal = match merge_cb {
        None => true,
        Some(cb) => cb as *const () == self_merge_file_ptr(),
    };

    if use_internal {
        internal_decode(src, delta, dst, expected_md5)
    } else {
        // External callback: manual skip check, call callback, read-back verify.
        external_decode(src, delta, dst, merge_cb.unwrap(), expected_md5)
    }
}

/// Source files above this size cannot be mmap'd safely inside the 32-bit
/// wrapper process (address-space exhaustion → OOM on multi-GB files). They
/// are delegated to the 64-bit `xdelta.exe` subprocess instead.
const LARGE_FILE_THRESHOLD: u64 = 256 * 1024 * 1024;

/// Locate the 64-bit apply subprocess: host-process directory first, then PATH.
fn xdelta_exe_path() -> Option<PathBuf> {
    // Prefer the directory next to the host process (delivery puts xdelta.exe
    // alongside the wrapper DLL and the host executable).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("xdelta.exe");
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    // Fallback: PATH lookup.
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join("xdelta.exe");
        if cand.exists() {
            return Some(cand);
        }
    }
    None
}

/// Apply a patch via the 64-bit `xdelta.exe` subprocess. Returns true when the
/// subprocess exits 0 (applied or skipped). `expected_md5` enables idempotent
/// skip + post-verify (handled by the subprocess's `--expect-after`).
fn subprocess_decode(
    src: &Path,
    delta: &Path,
    dst: &Path,
    expected_md5: Option<&str>,
) -> bool {
    let exe = match xdelta_exe_path() {
        Some(p) => p,
        None => {
            logln!("[subproc] xdelta.exe NOT FOUND");
            return false;
        }
    };
    let mut cmd = std::process::Command::new(&exe);
    cmd.creation_flags(CREATE_NO_WINDOW)
        .arg("apply")
        .arg("-s")
        .arg(src)
        .arg(delta)
        .arg("-o")
        .arg(dst);
    if let Some(md5) = expected_md5 {
        // Verify output via --expect-after, but skip the idempotent source-hash
        // pre-read: the wrapper only calls this for old sources that always get
        // patched, so the skip check would never fire and only costs a full
        // extra read of a multi-GB source file.
        cmd.arg("--no-skip-check").arg("--expect-after").arg(md5);
    }
    logln!("[subproc] {}", cmd_debug(&cmd));
    let sw = Stopwatch::start();
    match cmd.status() {
        Ok(st) => {
            let code = st.code().unwrap_or(-1);
            logln!("[timing] subproc decode exit={} ({:.1}ms)", code, sw.ms());
            if st.success() {
                logln!("[subproc] exit {}", code);
            } else {
                logln!("[error] subprocess xdelta.exe exit {} for {}", code, dst.display());
            }
            st.success()
        }
        Err(e) => {
            logln!("[timing] subproc decode spawn error ({:.1}ms)", sw.ms());
            logln!(
                "[error] subprocess xdelta.exe spawn error: {} for {}",
                e,
                dst.display()
            );
            false
        }
    }
}

/// In-place apply via the 64-bit subprocess (`xdelta.exe apply --in-place`).
/// Used for large sources that the 32-bit wrapper cannot mmap.
fn subprocess_decode_in_place(src: &Path, delta: &Path, expected_md5: Option<&str>) -> bool {
    let exe = match xdelta_exe_path() {
        Some(p) => p,
        None => {
            logln!("[subproc] xdelta.exe NOT FOUND");
            return false;
        }
    };
    let mut cmd = std::process::Command::new(&exe);
    cmd.creation_flags(CREATE_NO_WINDOW)
        .arg("apply")
        .arg("--in-place")
        .arg("-s")
        .arg(src)
        .arg(delta);
    if let Some(md5) = expected_md5 {
        cmd.arg("--expect-after").arg(md5);
    }
    logln!("[subproc] {}", cmd_debug(&cmd));
    let sw = Stopwatch::start();
    match cmd.status() {
        Ok(st) => {
            let code = st.code().unwrap_or(-1);
            logln!("[timing] subproc in-place exit={} ({:.1}ms)", code, sw.ms());
            if !st.success() {
                logln!(
                    "[error] subprocess in-place exit {} for {}",
                    code,
                    src.display()
                );
            }
            st.success()
        }
        Err(e) => {
            logln!("[timing] subproc in-place spawn error ({:.1}ms)", sw.ms());
            logln!(
                "[error] subprocess in-place spawn error: {} for {}",
                e,
                src.display()
            );
            false
        }
    }
}

fn cmd_debug(cmd: &std::process::Command) -> String {
    let mut s = String::new();
    for (i, a) in cmd.get_args().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&a.to_string_lossy());
    }
    s
}

/// Internal decode via `apply_paths_verified` (inline hashing, no read-back),
/// or the 64-bit subprocess for large sources.
fn internal_decode(src: &Path, delta: &Path, dst: &Path, expected_md5: Option<&str>) -> bool {
    // Large source: 32-bit process cannot mmap it safely; delegate.
    if file_size(src) > LARGE_FILE_THRESHOLD as i64 {
        return subprocess_decode(src, delta, dst, expected_md5);
    }

    // Lazy output file: skip path must not create the output file.
    let mut out = LazyFile::new(dst);

    let sw = Stopwatch::start();
    let result = match expected_md5 {
        Some(md5) => {
            let expected = rxdelta::checksum::decode_hex(md5);
            match expected {
                Some(bytes) => rxdelta::apply_paths_verified(
                    Some(src),
                    delta,
                    &mut out,
                    ChecksumAlgo::Md5,
                    None,
                    Some(&bytes),
                    &APPLY_OPTS,
                ),
                None => {
                    logln!(
                        "[error] cannot decode expected md5 {} for {}",
                        md5,
                        delta.display()
                    );
                    return false;
                }
            }
        }
        None => rxdelta::apply_paths(Some(src), delta, &mut out, &APPLY_OPTS).map(|stats| {
            ApplyOutcome::Applied {
                stats,
                checksums: Default::default(),
            }
        }),
    };

    match result {
        Ok(ApplyOutcome::Applied { .. }) | Ok(ApplyOutcome::Skipped { .. }) => {
            let _ = out.finish();
            logln!(
                "[timing] internal_decode src={:.1}MB -> {} ({:.1}ms)",
                file_size(src) as f64 / 1048576.0,
                dst.display(),
                sw.ms()
            );
            true
        }
        Err(e) => {
            logln!(
                "[error] xdelta apply failed src={} delta={} dst={}: {}",
                src.display(),
                delta.display(),
                dst.display(),
                e
            );
            let _ = out.finish();
            false
        }
    }
}

/// External callback: manual skip check + callback + read-back MD5 verify.
fn external_decode(
    src: &Path,
    delta: &Path,
    dst: &Path,
    merge_cb: MergeCb,
    expected_md5: Option<&str>,
) -> bool {
    // Idempotent skip: if src already hashes to the expected result, skip.
    if let Some(md5) = expected_md5 {
        if let Some(actual) = file_md5_upper(src) {
            if md5_hex_eq_ci(&actual, md5) {
                return true;
            }
        }
    }

    let src_w = path_to_wide(src);
    let delta_w = path_to_wide(delta);
    let dst_w = path_to_wide(dst);
    let cbsw = Stopwatch::start();
    let ok = unsafe { merge_cb(src_w.as_ptr(), delta_w.as_ptr(), dst_w.as_ptr()) };
    logln!(
        "[timing] external_decode callback {} ({:.1}ms)",
        dst.display(),
        cbsw.ms()
    );
    if ok == 0 {
        logln!(
            "[error] merge callback returned failure for {}",
            dst.display()
        );
        return false;
    }

    // Post-verify by reading back the output.
    if let Some(md5) = expected_md5 {
        if let Some(actual) = file_md5_upper(dst) {
            if !md5_hex_eq_ci(&actual, md5) {
                logln!(
                    "[error] MD5 mismatch: got={} want={} ({})",
                    actual,
                    md5,
                    dst.display()
                );
                return false;
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Self-reference mergeCb detection
// ---------------------------------------------------------------------------

/// Address of our own exported `MergeFile`. Because the `.def` alias maps
/// `MergeFile = _MergeFile@12` directly to the same function body (no export
/// thunk), the pointer a caller gets from `GetProcAddress("MergeFile")` is
/// byte-identical to this address — so direct comparison works, and it is also
/// independent of the module's file name.
fn self_merge_file_ptr() -> *const () {
    crate::MergeFile as *const ()
}

// ---------------------------------------------------------------------------
// Lazy output writer (deferred file creation)
// ---------------------------------------------------------------------------

/// Wraps `&mut impl Write` used by xdelta decode; only creates the file on
/// first write so skip/failure paths leave nothing behind.
struct LazyFile<'a> {
    path: &'a Path,
    file: Option<BufWriter<File>>,
}

impl<'a> LazyFile<'a> {
    fn new(path: &'a Path) -> Self {
        LazyFile { path, file: None }
    }

    fn finish(&mut self) -> std::io::Result<()> {
        if let Some(f) = self.file.as_mut() {
            f.flush()
        } else {
            Ok(())
        }
    }
}

impl<'a> Write for LazyFile<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.file.is_none() {
            self.file = Some(BufWriter::with_capacity(1 << 20, File::create(self.path)?));
        }
        self.file.as_mut().unwrap().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(f) = self.file.as_mut() {
            f.flush()
        } else {
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Public single-file entry
// ---------------------------------------------------------------------------

/// `MergeFile(src, delta, dst)`.
pub fn merge_file(src: *const u16, delta: *const u16, dst: *const u16, merge_cb: Option<MergeCb>) -> Result<(), ()> {
    unsafe {
        let src = u16_to_path(src).ok_or(())?;
        let delta = u16_to_path(delta).ok_or(())?;
        let dst = u16_to_path(dst).ok_or(())?;
        if merge_file_internal(&src, &delta, &dst, merge_cb, None) {
            Ok(())
        } else {
            Err(())
        }
    }
}

// ---------------------------------------------------------------------------
// Directory merge
// ---------------------------------------------------------------------------

/// Count total commands = sum of the four command-section entry counts.
fn count_patch_total_commands(dat: &PatchDat) -> i32 {
    (dat.count("EmptyPathInfo")
        + dat.count("DelPathInfo")
        + dat.count("NewPathInfo")
        + dat.count("DeltaPathInfo")) as i32
}

/// `MergeDirCustomDiffV2` core. `cmd_cb` may be null (MergeDir/CustomDiff).
pub fn merge_dir(
    src_dir: *const u16,
    patch_dir: *const u16,
    dst_dir: *const u16,
    file_cb: *const (),
    cmd_cb: *const (),
    merge_cb: *const (),
) -> bool {
    let file_cb: Option<FileProgressCb> = unsafe { std::mem::transmute(file_cb) };
    let cmd_cb: Option<CmdProgressCb> = unsafe { std::mem::transmute(cmd_cb) };
    let merge_cb: Option<MergeCb> = unsafe { std::mem::transmute(merge_cb) };

    unsafe {
        let src = match u16_to_path(src_dir) {
            Some(p) => p,
            None => return false,
        };
        let patch = match u16_to_path(patch_dir) {
            Some(p) => p,
            None => return false,
        };
        let eff_dst = match u16_to_path(dst_dir) {
            Some(p) => p,
            None => src.clone(),
        };

        // Parse dat from patch dir (needed before copy to know which files to skip).
        let dat_path = patch.join("patch_delta_direct.dat");
        let raw = match fs::read(&dat_path) {
            Ok(b) => b,
            Err(e) => {
                logln!(
                    "[error] cannot read dat file {}: {}",
                    dat_path.display(),
                    e
                );
                return false;
            }
        };
        let dat = match dat::parse(&raw) {
            Ok(d) => d,
            Err(e) => {
                logln!("[error] dat parse failed: {e} ({})", dat_path.display());
                return false;
            }
        };

        if !paths_equal(&src, &eff_dst) {
            // Copy whole tree src → dst first (xcopy /e /y equivalent).
            // Delta-patched files are skipped: their dst copy would be
            // immediately overwritten by process_delta. This removes up to the
            // entire tree-copy cost (the FFXIV dat covers 100% of bytes).
            let skip: std::collections::HashSet<PathBuf> = dat
                .entries("DeltaPathInfo")
                .iter()
                .map(|(k, _)| PathBuf::from(k))
                .collect();
            let csw = Stopwatch::start();
            if !copy_tree(&src, &eff_dst, Some(&skip)) {
                return false;
            }
            logln!("[phase] tree copy {:.1}s", csw.ms() / 1000.0);
        }

        let total = count_patch_total_commands(&dat);
        logln!("[phase] total commands = {}", total);
        let cur = Cell::new(0i32);

        let ctx = MergeCtx {
            src_dir: &src,
            patch_dir: &patch,
            dst_dir: &eff_dst,
            file_cb,
            cmd_cb,
            merge_cb,
            cur: &cur,
            total,
        };

        let p_sw = Stopwatch::start();
        let r = process_empty(&ctx, &dat)
            && process_delete(&ctx, &dat)
            && process_new(&ctx, &dat)
            && process_delta(&ctx, &dat);
        logln!("[phase] process* total {:.1}s", p_sw.ms() / 1000.0);
        r
    }
}

struct MergeCtx<'a> {
    src_dir: &'a Path,
    patch_dir: &'a Path,
    dst_dir: &'a Path,
    file_cb: Option<FileProgressCb>,
    cmd_cb: Option<CmdProgressCb>,
    merge_cb: Option<MergeCb>,
    cur: &'a Cell<i32>,
    total: i32,
}

/// Fire `file_cb(msg, ty)` with a formatted message.
unsafe fn emit_file(ctx: &MergeCtx, template: &str, file: &str, ty: i32) {
    if let Some(cb) = ctx.file_cb {
        let wide = msgs::format(template, file);
        cb(wide.as_ptr(), ty);
        logln!("[cb] {template}:{file} ty={ty}");
    } else {
        logln!("[cb] {template}:{file} ty={ty} (no callback)");
    }
}

/// Fire `cmd_cb(done, total)` and bump the counter.
fn emit_cmd(ctx: &MergeCtx) {
    if let Some(cb) = ctx.cmd_cb {
        let done = ctx.cur.get() + 1;
        ctx.cur.set(done);
        unsafe { cb(done, ctx.total) };
    }
}

/// Process `<EmptyPathInfo>`: copy each file src→dst.
fn process_empty(ctx: &MergeCtx, dat: &PatchDat) -> bool {
    let entries = dat.entries("EmptyPathInfo");
    let result_md5: std::collections::HashMap<&str, &str> = dat
        .entries("ResultMD5Info")
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    for (key, _value) in entries {
        let _ = result_md5; // (original also parses ResultMD5Info here; not used for skip)
        let src = join(ctx.src_dir, key);
        let dst = join(ctx.dst_dir, key);
        unsafe {
            emit_file(ctx, msgs::MSG_COPY_START, key, 0);
        }
        // Disk space pre-check (src size).
        let required = file_size(&src);
        if required > 0 && !check_disk_space(&dst, required) {
            logln!(
                "[error] disk space check failed: need {} bytes at {}",
                required,
                dst.display()
            );
            unsafe {
                emit_file(ctx, msgs::MSG_DISK_COPY, key, 2);
            }
            return false;
        }
        if !copy_file(&src, &dst) {
            unsafe {
                emit_file(ctx, msgs::MSG_COPY_FAIL, key, 1);
            }
            return false;
        }
        emit_cmd(ctx);
    }
    true
}

/// Process `<DelPathInfo>`: delete each file (from src_dir).
fn process_delete(ctx: &MergeCtx, dat: &PatchDat) -> bool {
    let entries = dat.entries("DelPathInfo");
    for (key, _value) in entries {
        let path = join(ctx.src_dir, key);
        if !file_exists(&path) {
            continue;
        }
        unsafe {
            emit_file(ctx, msgs::MSG_DELETE_START, key, 0);
        }
        if !delete_file(&path) {
            unsafe {
                emit_file(ctx, msgs::MSG_DELETE_FAIL, key, 3);
            }
            return false;
        }
        emit_cmd(ctx);
    }
    true
}

/// Process `<NewPathInfo>`: copy directory trees.
fn process_new(ctx: &MergeCtx, dat: &PatchDat) -> bool {
    let entries = dat.entries("NewPathInfo");
    for (key, value) in entries {
        let src = join(ctx.src_dir, key);
        let dst = join(ctx.dst_dir, value);
        unsafe {
            emit_file(ctx, msgs::MSG_COPY_START, key, 0);
        }
        if !copy_tree(&src, &dst, None) {
            unsafe {
                emit_file(ctx, msgs::MSG_COPY_FAIL, key, 1);
            }
            return false;
        }
        emit_cmd(ctx);
    }
    true
}

/// Process `<DeltaPathInfo>` with `<ResultMD5Info>` skip/verify.
fn process_delta(ctx: &MergeCtx, dat: &PatchDat) -> bool {
    let delta_entries = dat.entries("DeltaPathInfo");
    let result_md5: std::collections::HashMap<&str, &str> = dat
        .entries("ResultMD5Info")
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    for (name, patch_name) in delta_entries {
        unsafe {
            emit_file(ctx, msgs::MSG_MERGE_START, name, 0);
        }

        let base = join(ctx.src_dir, name);
        let delta = join(ctx.patch_dir, patch_name);
        let out = join(ctx.dst_dir, name);
        let expected = result_md5.get(name.as_str()).copied();

        if !apply_single_patch(ctx, name, &base, &delta, &out, expected) {
            return false;
        }
        emit_cmd(ctx);
    }
    true
}

/// One delta entry: skip check → disk check → decode → post-verify.
fn apply_single_patch(
    ctx: &MergeCtx,
    name: &str,
    base: &Path,
    delta: &Path,
    out: &Path,
    expected_md5: Option<&str>,
) -> bool {
    // Idempotent skip via expected result MD5 (internal path handles this
    // inside apply_paths_verified; external callback path does it manually).
    let use_internal = match ctx.merge_cb {
        None => true,
        Some(cb) => cb as *const () == self_merge_file_ptr(),
    };

    if use_internal {
        internal_single_patch(ctx, name, base, delta, out, expected_md5)
    } else {
        external_single_patch(ctx, name, base, delta, out, expected_md5)
    }
}

fn internal_single_patch(
    ctx: &MergeCtx,
    name: &str,
    base: &Path,
    delta: &Path,
    out: &Path,
    expected_md5: Option<&str>,
) -> bool {
    // Disk space check: in-place rewrite needs only the growth; a full write
    // needs the exact target size.
    let required = required_apply_bytes(base, delta, paths_equal(base, out));
    if required > 0 && !check_disk_space(out, required) {
        logln!(
            "[error] disk space check failed: need {} bytes at {}",
            required,
            out.display()
        );
        unsafe {
            emit_file(ctx, msgs::MSG_DISK_MERGE, name, 2);
        }
        return false;
    }
    // Ensure output parent exists + clear readonly on existing output.
    if !ensure_directory_path(out) {
        logln!(
            "[error] cannot create output directory for {}: {}",
            name,
            out.display()
        );
        unsafe {
            emit_file(ctx, msgs::MSG_MERGE_FAIL, name, 2);
        }
        return false;
    }
    clear_read_only(out);

    // In-place (dst == src, e.g. pDstDir == NULL): rewrite only changed slots.
    if paths_equal(base, out) {
        let sw = Stopwatch::start();
        let ok = in_place_apply(base, delta, expected_md5);
        logln!(
            "[apply-inplace] {:.1}MB {} -> {} in {:.0}ms",
            file_size(base) as f64 / 1048576.0,
            name,
            ok,
            sw.ms()
        );
        if !ok {
            unsafe {
                emit_file(ctx, msgs::MSG_MERGE_FAIL, name, 2);
            }
        }
        return ok;
    }

    // Large source: 32-bit process cannot mmap it safely; delegate to the
    // 64-bit subprocess (which also does skip + post-verify via --expect-after).
    if file_size(base) > LARGE_FILE_THRESHOLD as i64 {
        let sw = Stopwatch::start();
        let ok = subprocess_decode(base, delta, out, expected_md5);
        // Exit 0 means "applied" (output exists) or "skipped" (no output
        // written; with skip-copy, dst lacks the file, so produce from base).
        let final_ok = if ok {
            if file_exists(out) {
                true
            } else if !file_exists(base) {
                false
            } else {
                copy_file(base, out)
            }
        } else {
            false
        };
        logln!(
            "[apply] {:.1}MB {} subprocess -> {} (skip-copy {}) in {:.0}ms",
            file_size(base) as f64 / 1048576.0,
            name,
            final_ok,
            ok && !file_exists(out) && final_ok,
            sw.ms()
        );
        if final_ok {
            true
        } else {
            let _ = fs::remove_file(out);
            unsafe {
                emit_file(ctx, msgs::MSG_MERGE_FAIL, name, 2);
            }
            false
        }
    } else {
        let sw = Stopwatch::start();
        let ok = internal_small_patch(ctx, name, base, delta, out, expected_md5);
        logln!(
            "[apply] {:.1}MB {} internal -> {} in {:.0}ms",
            file_size(base) as f64 / 1048576.0,
            name,
            ok,
            sw.ms()
        );
        ok
    }
}

/// In-process decode for source files small enough for 32-bit mmap.
fn internal_small_patch(
    ctx: &MergeCtx,
    name: &str,
    base: &Path,
    delta: &Path,
    out: &Path,
    expected_md5: Option<&str>,
) -> bool {
    match expected_md5 {
        Some(md5) => {
            let expected = match rxdelta::checksum::decode_hex(md5) {
                Some(b) => b,
                None => {
                    unsafe {
                        emit_file(ctx, msgs::MSG_MERGE_FAIL, name, 2);
                    }
                    return false;
                }
            };
            let mut lazy = LazyFile::new(out);
            match rxdelta::apply_paths_verified(
                Some(base),
                delta,
                &mut lazy,
                ChecksumAlgo::Md5,
                None,
                Some(&expected),
                &APPLY_OPTS,
            ) {
                Ok(ApplyOutcome::Applied { .. }) => {
                    let _ = lazy.finish();
                    true
                }
                // Idempotent skip: base already at target; dst was not copied
                // (skip-copy optimization), so produce out from base.
                Ok(ApplyOutcome::Skipped { .. }) => {
                    let _ = lazy.finish();
                    if !file_exists(out) {
                        copy_file(base, out)
                    } else {
                        true
                    }
                }
                Err(e) => {
                    logln!(
                        "[error] xdelta apply failed for {} (src={} delta={}): {}",
                        name,
                        base.display(),
                        delta.display(),
                        e
                    );
                    let _ = fs::remove_file(out);
                    unsafe {
                        emit_file(ctx, msgs::MSG_MERGE_FAIL, name, 2);
                    }
                    false
                }
            }
        }
        None => {
            let mut lazy = LazyFile::new(out);
            match rxdelta::apply_paths(Some(base), delta, &mut lazy, &APPLY_OPTS) {
                Ok(_) => {
                    let _ = lazy.finish();
                    true
                }
                Err(e) => {
                    logln!(
                        "[error] xdelta apply failed for {} (src={} delta={}): {}",
                        name,
                        base.display(),
                        delta.display(),
                        e
                    );
                    let _ = fs::remove_file(out);
                    unsafe {
                        emit_file(ctx, msgs::MSG_MERGE_FAIL, name, 2);
                    }
                    false
                }
            }
        }
    }
}

fn external_single_patch(
    ctx: &MergeCtx,
    name: &str,
    base: &Path,
    delta: &Path,
    out: &Path,
    expected_md5: Option<&str>,
) -> bool {
    // Idempotent skip: base already at target. With the skip-copy optimization
    // dst may not contain the file yet, so produce it from base.
    if let Some(md5) = expected_md5 {
        if let Some(actual) = file_md5_upper(base) {
            if md5_hex_eq_ci(&actual, md5) {
                if !file_exists(out) {
                    return copy_file(base, out);
                }
                return true;
            }
        }
    }
    // Disk space check: external callback always does a full write, so it
    // needs the exact target size.
    let required = required_apply_bytes(base, delta, false);
    if required > 0 && !check_disk_space(out, required) {
        logln!(
            "[error] disk space check failed: need {} bytes at {}",
            required,
            out.display()
        );
        unsafe {
            emit_file(ctx, msgs::MSG_DISK_MERGE, name, 2);
        }
        return false;
    }
    if !ensure_directory_path(out) {
        logln!(
            "[error] cannot create output directory for {}: {}",
            name,
            out.display()
        );
        unsafe {
            emit_file(ctx, msgs::MSG_MERGE_FAIL, name, 2);
        }
        return false;
    }
    clear_read_only(out);

    let cb = match ctx.merge_cb {
        Some(cb) => cb,
        None => {
            unsafe {
                emit_file(ctx, msgs::MSG_MERGE_FAIL, name, 2);
            }
            return false;
        }
    };
    let base_w = path_to_wide(base);
    let delta_w = path_to_wide(delta);
    let out_w = path_to_wide(out);
    let cbsw = Stopwatch::start();
    let ok = unsafe { cb(base_w.as_ptr(), delta_w.as_ptr(), out_w.as_ptr()) };
    logln!(
        "[timing] external_single_patch callback {name} ({:.1}ms)",
        cbsw.ms()
    );
    if ok == 0 {
        logln!("[error] merge callback returned failure for {name} ({})", out.display());
        unsafe {
            emit_file(ctx, msgs::MSG_MERGE_FAIL, name, 2);
        }
        return false;
    }

    // Post-verify.
    if let Some(md5) = expected_md5 {
        unsafe {
            emit_file(ctx, msgs::MSG_VERIFY, name, 0);
        }
        if let Some(actual) = file_md5_upper(out) {
            if !md5_hex_eq_ci(&actual, md5) {
                logln!(
                    "[error] MD5 mismatch: got={} want={} ({})",
                    actual,
                    md5,
                    out.display()
                );
                unsafe {
                    emit_file(ctx, msgs::MSG_MERGE_FAIL, name, 2);
                }
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn xdelta3() -> &'static str {
        let e = std::env::var("XDELTA3");
        if let Ok(path) = e {
            return Box::leak(path.into_boxed_str());
        }
        "xdelta3"
    }

    fn make_patch(old: &Path, new: &Path, patch_path: &Path) {
        let st = Command::new(xdelta3())
            .arg("-e")
            .arg("-f")
            .arg("-s")
            .arg(old)
            .arg(new)
            .arg(patch_path)
            .output()
            .expect("run xdelta3 encode");
        assert!(
            st.status.success(),
            "xdelta3 encode failed: {}",
            String::from_utf8_lossy(&st.stderr)
        );
    }

    fn md5_hex(p: &Path) -> String {
        file_md5_upper(p).unwrap()
    }

    #[test]
    fn self_merge_file_ptr_matches_export() {
        // crate::MergeFile must be a valid non-null code address, and the
        // .def alias maps GetProcAddress("MergeFile") to the same body.
        let p = self_merge_file_ptr();
        assert!(!p.is_null());
        assert_eq!(p, crate::MergeFile as *const ());
    }

    #[test]
    fn merge_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.bin");
        let new = dir.path().join("new.bin");
        let patch = dir.path().join("patch.vcdiff");
        let out = dir.path().join("out.bin");

        fs::write(&old, vec![0x41u8; 4096]).unwrap();
        fs::write(&new, vec![0x42u8; 8192]).unwrap();
        make_patch(&old, &new, &patch);

        let (old_w, patch_w, out_w) = (path_to_wide(&old), path_to_wide(&patch), path_to_wide(&out));
        let ok = merge_file(old_w.as_ptr(), patch_w.as_ptr(), out_w.as_ptr(), None);
        assert!(ok.is_ok());
        assert_eq!(fs::read(&out).unwrap(), fs::read(&new).unwrap());
    }

    #[test]
    fn merge_file_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.bin");
        let target = dir.path().join("t.bin");
        let patch = dir.path().join("patch.vcdiff");

        fs::write(&f, vec![0x41u8; 4096]).unwrap();
        fs::write(&target, vec![0x42u8; 8192]).unwrap();
        make_patch(&f, &target, &patch);

        let (fw, pw, dw) = (path_to_wide(&f), path_to_wide(&patch), path_to_wide(&f));
        let ok = merge_file(fw.as_ptr(), pw.as_ptr(), dw.as_ptr(), None);
        assert!(ok.is_ok());
        assert_eq!(fs::read(&f).unwrap(), fs::read(&target).unwrap());
        assert!(!dir.path().join("f.bin.tmp").exists());
    }

    #[test]
    fn merge_file_decode_failure_cleans_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.bin");
        let patch = dir.path().join("bad.vcdiff");

        fs::write(&f, vec![0x41u8; 64]).unwrap();
        fs::write(&patch, b"not a vcdiff").unwrap();

        let (fw, pw, dw) = (path_to_wide(&f), path_to_wide(&patch), path_to_wide(&f));
        let ok = merge_file(fw.as_ptr(), pw.as_ptr(), dw.as_ptr(), None);
        assert!(ok.is_err());
        assert!(!dir.path().join("f.bin.tmp").exists());
    }

    extern "stdcall" fn test_file_cb(_msg: *const u16, _ty: i32) {}
    extern "stdcall" fn test_cmd_cb(_done: i32, _total: i32) {}

    fn dat_content(delta_entries: &[(&str, &str)], result_md5: &[(&str, &str)]) -> String {
        let mut s = String::from("<XMLROOT>\n<DeltaPathInfo>\n");
        for (k, v) in delta_entries {
            s.push_str(&format!("<DeltaPathSubItem Key=\"{}\" Value=\"{}\"/>\n", k, v));
        }
        s.push_str("</DeltaPathInfo>\n<ResultMD5Info>\n");
        for (k, v) in result_md5 {
            s.push_str(&format!("<ResultMD5SubItem Key=\"{}\" Value=\"{}\"/>\n", k, v));
        }
        s.push_str("</ResultMD5Info>\n</XMLROOT>\n");
        s
    }

    #[test]
    fn merge_dir_custom_diff_v2_e2e() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let patch = dir.path().join("patch");
        let dst = dir.path().join("dst");
        fs::create_dir_all(&src.join("game")).unwrap();
        fs::create_dir_all(&patch.join("Pkg/game")).unwrap();

        let old = src.join("game/ffxiv_dx11.exe");
        let target = vec![0x55u8; 16384];
        fs::write(&old, vec![0x41u8; 16384]).unwrap();

        let new_tmp = dir.path().join("new_tmp.bin");
        fs::write(&new_tmp, &target).unwrap();
        let delta = patch.join("Pkg/game/ffxiv_dx11.exe.delta");
        make_patch(&old, &new_tmp, &delta);

        let result_md5 = md5_hex(&new_tmp);
        let dat = dat_content(
            &[("game/ffxiv_dx11.exe", "Pkg/game/ffxiv_dx11.exe.delta")],
            &[("game/ffxiv_dx11.exe", result_md5.as_str())],
        );
        fs::write(patch.join("patch_delta_direct.dat"), dat).unwrap();

        let (src_w, patch_w, dst_w) = (path_to_wide(&src), path_to_wide(&patch), path_to_wide(&dst));
        let ok = merge_dir(
            src_w.as_ptr(),
            patch_w.as_ptr(),
            dst_w.as_ptr(),
            test_file_cb as *const (),
            test_cmd_cb as *const (),
            std::ptr::null(),
        );
        assert!(ok, "MergeDirCustomDiffV2 failed");
        let merged = dst.join("game/ffxiv_dx11.exe");
        assert_eq!(fs::read(&merged).unwrap(), target);
    }

    #[test]
    fn merge_dir_skips_already_patched() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let patch = dir.path().join("patch");
        let dst = dir.path().join("dst");
        fs::create_dir_all(&src.join("game")).unwrap();
        fs::create_dir_all(&patch.join("Pkg/game")).unwrap();

        let target = vec![0x77u8; 8192];
        fs::write(src.join("game/ffxiv_dx11.exe"), &target).unwrap();

        let old_tmp = dir.path().join("old_tmp.bin");
        fs::write(&old_tmp, vec![0x41u8; 8192]).unwrap();
        let new_tmp = dir.path().join("new_tmp.bin");
        fs::write(&new_tmp, vec![0x42u8; 8192]).unwrap();
        let delta = patch.join("Pkg/game/ffxiv_dx11.exe.delta");
        make_patch(&old_tmp, &new_tmp, &delta);

        let current_md5 = md5_hex(&src.join("game/ffxiv_dx11.exe"));
        let dat = dat_content(
            &[("game/ffxiv_dx11.exe", "Pkg/game/ffxiv_dx11.exe.delta")],
            &[("game/ffxiv_dx11.exe", current_md5.as_str())],
        );
        fs::write(patch.join("patch_delta_direct.dat"), dat).unwrap();

        let (src_w, patch_w, dst_w) = (path_to_wide(&src), path_to_wide(&patch), path_to_wide(&dst));
        let ok = merge_dir(
            src_w.as_ptr(),
            patch_w.as_ptr(),
            dst_w.as_ptr(),
            test_file_cb as *const (),
            test_cmd_cb as *const (),
            std::ptr::null(),
        );
        assert!(ok);
        let dst_file = dst.join("game/ffxiv_dx11.exe");
        if dst_file.exists() {
            assert_eq!(fs::read(&dst_file).unwrap(), target);
        }
    }

    extern "stdcall" fn external_merge_cb(src: *const u16, delta: *const u16, out: *const u16) -> i32 {
        unsafe {
            let src_p = u16_to_path(src).unwrap();
            let delta_p = u16_to_path(delta).unwrap();
            let out_p = u16_to_path(out).unwrap();
            let mut lazy = LazyFile::new(&out_p);
            match rxdelta::apply_paths(Some(&src_p), &delta_p, &mut lazy, &APPLY_OPTS) {
                Ok(_) => {
                    let _ = lazy.finish();
                    1
                }
                Err(_) => 0,
            }
        }
    }

    #[test]
    fn merge_dir_external_merge_cb() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let patch = dir.path().join("patch");
        let dst = dir.path().join("dst");
        fs::create_dir_all(&src.join("game")).unwrap();
        fs::create_dir_all(&patch.join("Pkg/game")).unwrap();

        let old = src.join("game/ffxiv_dx11.exe");
        fs::write(&old, vec![0x41u8; 4096]).unwrap();
        let target = vec![0x42u8; 4096];
        let new_tmp = dir.path().join("new_tmp.bin");
        fs::write(&new_tmp, &target).unwrap();
        let delta = patch.join("Pkg/game/ffxiv_dx11.exe.delta");
        make_patch(&old, &new_tmp, &delta);

        let result_md5 = md5_hex(&new_tmp);
        let dat = dat_content(
            &[("game/ffxiv_dx11.exe", "Pkg/game/ffxiv_dx11.exe.delta")],
            &[("game/ffxiv_dx11.exe", result_md5.as_str())],
        );
        fs::write(patch.join("patch_delta_direct.dat"), dat).unwrap();

        let (src_w, patch_w, dst_w) = (path_to_wide(&src), path_to_wide(&patch), path_to_wide(&dst));
        let ok = merge_dir(
            src_w.as_ptr(),
            patch_w.as_ptr(),
            dst_w.as_ptr(),
            test_file_cb as *const (),
            test_cmd_cb as *const (),
            external_merge_cb as *const (),
        );
        assert!(ok);
        assert_eq!(fs::read(dst.join("game/ffxiv_dx11.exe")).unwrap(), target);
    }
}

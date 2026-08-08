//! Verification against the real FFXIV patch sample captured during a debug
//! run. These tests only run when the sample is present; the sample paths come
//! from environment variables and default to empty, so they skip silently
//! elsewhere and CI stays independent of any machine-specific path.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rxdelta::{ApplyOptions, Checksum, Md5, apply_paths_in_place, encode_hex, scan_layout};

fn env_path(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// Root of the single-file sample (contains `src/`, `patch/Pkg/`, `dst/`),
/// from `XD_SAMPLE_DIR`. Empty when unset, so the sample tests skip.
fn sample_root() -> String {
    env_path("XD_SAMPLE_DIR")
}

fn sample_file(rel: &str) -> String {
    let root = sample_root();
    if root.is_empty() {
        String::new()
    } else {
        format!("{root}/{rel}")
    }
}

fn src_path() -> String {
    sample_file("src/game/sqpack/ex3/030300.win32.dat0")
}

fn delta_path() -> String {
    sample_file("patch/Pkg/game/sqpack/ex3/030300.win32.dat0.delta")
}

fn dst_path() -> String {
    sample_file("dst/game/sqpack/ex3/030300.win32.dat0")
}

fn patch_dir() -> String {
    env_path("XD_PATCH_DIR")
}

fn old_dir() -> String {
    env_path("XD_OLD_DIR")
}

fn new_dir() -> String {
    env_path("XD_NEW_DIR")
}

fn work_root() -> PathBuf {
    env_path("XD_VERIFY_DIR").into()
}

fn sample_present() -> bool {
    !src_path().is_empty() && !delta_path().is_empty() && !dst_path().is_empty()
}

fn md5_file(path: &Path) -> String {
    let f = std::fs::File::open(path).unwrap();
    let mut r = std::io::BufReader::with_capacity(1 << 20, f);
    let mut h = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    encode_hex(&h.digest())
}

#[test]
fn real_sample_layout_matches_manual_analysis() {
    if !sample_present() {
        eprintln!("skipping: real FFXIV sample not present");
        return;
    }
    let delta = std::fs::read(delta_path()).unwrap();
    let layout = scan_layout(&delta, rxdelta::DEFAULT_MAX_WINDOW).unwrap();
    assert_eq!(layout.windows.len(), 123_437);
    assert_eq!(layout.target_len, 2_022_389_632);
    assert_eq!(layout.skipped_windows, 123_374);
    assert_eq!(layout.changed_windows, 63);
}

#[test]
fn real_sample_in_place_matches_known_good_output() {
    if !sample_present() {
        eprintln!("skipping: real FFXIV sample not present");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src_copy = dir.path().join("030300.win32.dat0");
    std::fs::copy(src_path(), &src_copy).unwrap();

    let stats = apply_paths_in_place(&src_copy, Path::new(&delta_path()), &ApplyOptions::default()).unwrap();
    assert_eq!(stats.skipped_windows, 123_374);
    assert_eq!(stats.written_windows, 63);
    assert_eq!(stats.windows, 123_437);

    let got = md5_file(&src_copy);
    let want = md5_file(Path::new(&dst_path()));
    assert_eq!(got, want, "in-place result must equal the known-good patched output");
}

/// Cross-validate the Rust window parser against every real delta in the
/// patch set. Two layers:
///   1. Internal consistency (must hold for all deltas): the parsed windows
///      are non-empty and `sum(target_len) == layout.target_len`.
///   2. Size cross-check: for deltas that directly produced the post-patch
///      files under `2026.07.16.0001.0000`, `layout.target_len` must equal the
///      new file size. A few files were further modified by later updates, so
///      the exact-match set may not cover them; the mismatch list is surfaced
///      in the assertion message.
#[test]
fn scan_every_real_delta_is_well_formed() {
    let patch_dir = patch_dir();
    let new_dir = new_dir();
    if patch_dir.is_empty()
        || new_dir.is_empty()
        || !Path::new(&patch_dir).exists()
        || !Path::new(&new_dir).exists()
    {
        eprintln!("skipping: real FFXIV patch set not present");
        return;
    }

    let mut total_windows = 0usize;
    let mut exact = 0usize;
    let mut mismatches: Vec<String> = Vec::new();
    for entry in walk_deltas(Path::new(&patch_dir)) {
        let delta = std::fs::read(&entry).unwrap();
        let layout = scan_layout(&delta, rxdelta::DEFAULT_MAX_WINDOW).unwrap();
        assert!(!layout.windows.is_empty(), "empty layout for {}", entry.display());
        let sum: u64 = layout.windows.iter().map(|w| w.win.target_len as u64).sum();
        assert_eq!(
            sum, layout.target_len,
            "target sum mismatch for {}",
            entry.display()
        );
        total_windows += layout.windows.len();

        let rel = entry.strip_prefix(&patch_dir).unwrap();
        let new_path = Path::new(&new_dir).join(rel.with_extension(""));
        let Ok(meta) = std::fs::metadata(&new_path) else {
            continue;
        };
        if layout.target_len == meta.len() {
            exact += 1;
        } else {
            mismatches.push(format!(
                "{} (delta_target={}, new_size={})",
                rel.display(),
                layout.target_len,
                meta.len()
            ));
        }
    }
    assert!(total_windows > 1_000_000, "expected ~2.2M windows total, got {total_windows}");
    assert!(
        exact >= 20,
        "expected most real deltas to match their new file size, got {exact}/28; mismatches: {}",
        mismatches.join("; ")
    );
}

fn walk_deltas(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "delta") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// Timing breakdown for the in-place apply + output MD5. The source-file copy
/// is NOT timed (reported separately); only the patch apply and the MD5 pass
/// are measured. Run with `cargo test --test in_place_real -- --ignored`.
#[test]
#[ignore]
fn time_apply_and_md5() {
    let old_dir = old_dir();
    let patch_dir = patch_dir();
    if old_dir.is_empty()
        || patch_dir.is_empty()
        || !Path::new(&old_dir).exists()
        || !Path::new(&patch_dir).exists()
    {
        eprintln!("skipping: real FFXIV patch set not present (set XD_OLD_DIR / XD_PATCH_DIR)");
        return;
    }
    let work_root = work_root();
    if work_root.as_os_str().is_empty() {
        eprintln!("skipping: XD_VERIFY_DIR not set");
        return;
    }

    let mut n = 0usize;
    for entry in walk_deltas(Path::new(&patch_dir)) {
        let rel = entry.strip_prefix(&patch_dir).unwrap();
        let old = Path::new(&old_dir).join(rel.with_extension(""));
        if !old.exists() {
            continue;
        }
        let src = work_root.join(format!("_timing_{n}.src"));
        let _ = std::fs::remove_file(&src);

        let copy_t0 = std::time::Instant::now();
        std::fs::copy(&old, &src).unwrap();
        let copy_s = copy_t0.elapsed();

        let apply_t0 = std::time::Instant::now();
        let stats =
            apply_paths_in_place(&src, &entry, &ApplyOptions::default()).unwrap();
        let apply_ms = apply_t0.elapsed().as_millis();

        let md5_t0 = std::time::Instant::now();
        let _ = md5_file(&src);
        let md5_ms = md5_t0.elapsed().as_millis();

        let _ = std::fs::remove_file(&src);
        eprintln!(
            "{:45} copy={:>5.1}s  apply={:>6}ms (write {:>8}B)  md5={:>6}ms  apply+md5={:>6}ms",
            rel.display(),
            copy_s.as_secs_f64(),
            apply_ms,
            stats.written_bytes,
            md5_ms,
            apply_ms + md5_ms,
        );
        n += 1;
    }
}
/// Heavy end-to-end verification over every real delta whose old source file
/// exists under `2026.07.16.0001.0000` (the pre-patch version). Runs the
/// in-place apply on a COPY of the source, then a full rewrite to a separate
/// output, and requires identical output hashes.
///
/// Safety: the original source tree is only ever READ; the in-place apply and
/// the full rewrite both work on copies in the work directory. Each file's
/// temp copies are deleted right after verification, so peak disk usage is one
/// file's (source copy + output), not the whole tree.
///
/// The work directory comes from the `XD_VERIFY_DIR` env var and must be set.
/// Reads/writes tens of GB, so the test is ignored by default; run with
/// `cargo test --test in_place_real -- --ignored`.
#[test]
#[ignore]
fn every_real_delta_in_place_matches_full_apply() {
    let old_dir = old_dir();
    let patch_dir = patch_dir();
    if old_dir.is_empty()
        || patch_dir.is_empty()
        || !Path::new(&old_dir).exists()
        || !Path::new(&patch_dir).exists()
    {
        eprintln!("skipping: real FFXIV patch set not present (set XD_OLD_DIR / XD_PATCH_DIR)");
        return;
    }
    let work_root = work_root();
    if work_root.as_os_str().is_empty() {
        eprintln!("skipping: XD_VERIFY_DIR not set");
        return;
    }
    std::fs::create_dir_all(&work_root).unwrap();

    let mut checked = 0usize;
    for entry in walk_deltas(Path::new(&patch_dir)) {
        let rel = entry.strip_prefix(&patch_dir).unwrap();
        let old = Path::new(&old_dir).join(rel.with_extension(""));
        if !old.exists() {
            eprintln!("skip: no source for {}", rel.display());
            continue;
        }
        let src = work_root.join(format!("_verify_{checked}.src"));
        let out = work_root.join(format!("_verify_{checked}.out"));
        // Remove stale leftovers from a previous interrupted run.
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);

        std::fs::copy(&old, &src).unwrap();

        let stats =
            apply_paths_in_place(&src, &entry, &ApplyOptions::default()).unwrap();
        let mut f = std::fs::File::create(&out).unwrap();
        rxdelta::apply_paths(Some(&old), &entry, &mut f, &ApplyOptions::default()).unwrap();
        f.flush().unwrap();

        let a = md5_file(&src);
        let b = md5_file(&out);
        assert_eq!(
            a, b,
            "in-place result != full-apply result for {}",
            rel.display()
        );
        eprintln!(
            "ok: {} (target={}B, in_place_written={}B, skipped_win={})",
            rel.display(),
            stats.target_len,
            stats.written_bytes,
            stats.skipped_windows
        );
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);
        checked += 1;
    }
    assert!(checked >= 25, "expected most real deltas verified, got {checked}");
}
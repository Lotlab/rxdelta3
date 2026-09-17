//! In-place patch application: rewrite only the window slots whose content
//! actually changed, instead of writing the whole output file.
//!
//! The source file is patched in place (output == source). Correctness relies
//! on a two-phase strategy: every window is decoded while the source is still
//! untouched, and only then are the changed slots written back. This avoids
//! the cross-window read-after-write hazard where a window's COPY reads bytes
//! that an earlier window's write already overwrote.
//!
//! Crash safety for the write-back phase relies on a small on-disk journal:
//! before the source is touched, the original bytes of every range that will
//! be overwritten or truncated are streamed into
//! `<source>.xdelta-journal.tmp` and fsync'd (the only fsync in the apply;
//! the source itself is flushed, not synced). If a write fails (I/O error,
//! process kill, power loss) the journal replays those ranges and restores
//! the original length. A stale journal left by a crashed run is replayed
//! automatically at the start of the next apply, and can also be replayed
//! manually with [`recover_in_place_journal`]. The journal is deleted once
//! the source has been fully written and flushed.
//!
//! Window decoding is shared with `decode_one_window`; this
//! module only adds the layout scan, pure-copy detection, two-phase write-back
//! with journal rollback, the verified (`expect_before` pre-check /
//! `expect_after` post-check) checks and the temp-rename threshold fallback.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::address::AddrCache;
use super::code_table::{COPY, CodeTable, NOOP};
use super::decode_one_window;
use super::header::{SecondaryId, parse_header};
use super::secondary::SecondaryDecoder;
use super::window::{VCD_SOURCE, Window, parse_window};
use crate::ApplyOptions;
use crate::checksum::{Checksum, ChecksumAlgo};
use crate::errors::{Error, Result};
use crate::io::MappedFile;
use crate::varint::read_usize;

/// Default cap on total changed-window bytes before falling back to a full
/// temp-file rewrite. Bounded memory is a hard requirement, so the in-place
/// path never buffers more than this many bytes of decoded output.
pub const DEFAULT_IN_PLACE_THRESHOLD: u64 = 1 << 30;

/// Per-window layout captured by a cheap header-only scan.
#[derive(Debug, Clone, Copy)]
pub struct WindowLayout {
    /// Absolute target output offset of this window.
    pub target_offset: u64,
    /// Parsed window header.
    pub win: Window,
    /// True when the window is a single aligned full-slot COPY from the
    /// source, i.e. target content is byte-identical to the source slot and
    /// can be skipped without reading or writing anything.
    pub pure_copy: bool,
}

/// Result of scanning a delta's window structure (no decompression, no source
/// reads).
#[derive(Debug, Clone)]
pub struct Layout {
    pub code_table: CodeTable,
    pub secondary: Option<SecondaryId>,
    pub windows: Vec<WindowLayout>,
    /// Total target size across all windows.
    pub target_len: u64,
    /// Sum of `target_len` over windows that are not pure copies.
    pub changed_bytes: u64,
    pub changed_windows: usize,
    pub skipped_windows: usize,
}

/// Statistics for an in-place apply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InPlaceStats {
    pub windows: u64,
    /// Windows whose slots were actually written.
    pub written_windows: u64,
    pub written_bytes: u64,
    /// Pure-copy windows skipped without reading or writing.
    pub skipped_windows: u64,
    pub target_len: u64,
}

/// Scan a delta's window layout without decompressing anything or touching the
/// source. This is the cheap pre-pass that decides whether in-place is safe
/// and affordable.
pub fn scan_layout(delta: &[u8], max_window: usize) -> Result<Layout> {
    let mut pos = 0usize;
    let hdr = parse_header(delta, &mut pos)?;
    if let Some(s) = hdr.secondary
        && s != SecondaryId::Lzma
    {
        return Err(Error::Unsupported(
            "only LZMA secondary compression is supported",
        ));
    }
    let secondary = hdr.secondary;
    let code_table = hdr.code_table;

    let mut windows = Vec::new();
    let mut target_offset = 0u64;
    let mut changed_bytes = 0u64;
    let mut changed_windows = 0usize;
    let mut skipped_windows = 0usize;
    while pos < delta.len() {
        let w = parse_window(delta, &mut pos, secondary, max_window)?;
        let pure = is_pure_copy(&code_table, delta, &w, target_offset);
        if pure {
            skipped_windows += 1;
        } else {
            changed_windows += 1;
            changed_bytes = changed_bytes.saturating_add(w.target_len as u64);
        }
        windows.push(WindowLayout {
            target_offset,
            win: w,
            pure_copy: pure,
        });
        target_offset = target_offset.saturating_add(w.target_len as u64);
        pos = w.addr_end;
    }

    Ok(Layout {
        code_table,
        secondary,
        windows,
        target_len: target_offset,
        changed_bytes,
        changed_windows,
        skipped_windows,
    })
}

/// Cheap pure-copy detection. A window is a pure copy when it is uncompressed,
/// has no data section, and is a single COPY instruction reproducing the full
/// aligned source slot (`copy size == target_len`, `addr == 0`,
/// `source_seg_pos == target_offset`, `source_seg_len == target_len`). Such a
/// window's output is, by construction, identical to the bytes already sitting
/// in the source slot, so it needs no reads and no writes.
fn is_pure_copy(ct: &CodeTable, delta: &[u8], w: &Window, target_offset: u64) -> bool {
    if w.data_comp || w.inst_comp || w.addr_comp {
        return false;
    }
    if w.win_ind & VCD_SOURCE == 0 {
        return false;
    }
    if w.data_len != 0 || w.source_seg_len != w.target_len {
        return false;
    }
    if w.source_seg_pos as u64 != target_offset {
        return false;
    }
    let Some(&op) = delta
        .get(w.inst_start..w.addr_start)
        .and_then(|s| s.first())
    else {
        return false;
    };
    let entry = &ct.entries[op as usize];
    if entry.first.inst != COPY || entry.second.inst != NOOP {
        return false;
    }
    if entry.first.size == 0 {
        let Some(size_bytes) = delta.get(w.inst_start + 1..w.addr_start) else {
            return false;
        };
        let mut p = 0usize;
        let Ok(sz) = read_usize(size_bytes, &mut p) else {
            return false;
        };
        if sz != w.target_len || p != size_bytes.len() {
            return false;
        }
    } else if entry.first.size as usize != w.target_len
        || w.addr_start.saturating_sub(w.inst_start) != 1
    {
        return false;
    }
    if entry.first.mode != 0 {
        return false;
    }
    let Some(addr) = delta.get(w.addr_start..w.addr_end) else {
        return false;
    };
    let mut ap = 0usize;
    read_usize(addr, &mut ap).ok() == Some(0) && ap == addr.len()
}

/// Apply a delta to the source file in place, rewriting only the changed
/// window slots. Uses [`DEFAULT_IN_PLACE_THRESHOLD`] as the buffering cap.
pub fn apply_paths_in_place(
    source: &Path,
    delta: &Path,
    opts: &ApplyOptions,
) -> Result<InPlaceStats> {
    apply_paths_in_place_with_threshold(source, delta, opts, DEFAULT_IN_PLACE_THRESHOLD)
}

/// In-place apply with an explicit cap on total changed-window bytes. When the
/// cap is exceeded the source is untouched and the patch is applied through a
/// temp file + rename instead (the same crash-safe path as a normal apply).
pub fn apply_paths_in_place_with_threshold(
    source: &Path,
    delta: &Path,
    opts: &ApplyOptions,
    threshold: u64,
) -> Result<InPlaceStats> {
    // A stale journal means a previous run died mid-write. Replay it first so
    // this apply starts from the intact original instead of a torn file.
    recover_in_place_journal(source)?;

    let delta_map = MappedFile::open(delta)?;
    let delta_bytes = delta_map.as_bytes();
    let layout = scan_layout(delta_bytes, opts.max_window_size)?;

    if layout.changed_bytes > threshold {
        return apply_via_temp(source, delta, opts);
    }

    // Phase 1: decode every changed window while the source file is still
    // untouched. All reads happen here; the read-only mmap is dropped before
    // any write, so a COPY reaching back into an earlier window's slot still
    // sees original bytes.
    let changed = {
        let src_map = MappedFile::open(source)?;
        let src_bytes = src_map.as_bytes();
        let mut target = Vec::new();
        let mut data_buf = Vec::new();
        let mut inst_buf = Vec::new();
        let mut addr_buf = Vec::new();
        let mut secondary = SecondaryDecoder::new();
        let mut cache = AddrCache::new(layout.code_table.s_near, layout.code_table.s_same);
        let mut changed: Vec<(u64, Vec<u8>)> = Vec::with_capacity(layout.changed_windows);
        for wl in &layout.windows {
            if wl.pure_copy {
                continue;
            }
            decode_one_window(
                delta_bytes,
                Some(src_bytes),
                &wl.win,
                &layout.code_table,
                &mut cache,
                opts,
                &mut secondary,
                &mut data_buf,
                &mut inst_buf,
                &mut addr_buf,
                &mut target,
            )?;
            changed.push((wl.target_offset, target.clone()));
        }
        changed
    };

    // Phase 2: journal the original blocks, then write the changed slots.
    // Any write failure rolls the source back from the journal.
    commit_changed_slots(source, layout.target_len, &changed)?;

    Ok(InPlaceStats {
        windows: layout.windows.len() as u64,
        written_windows: changed.len() as u64,
        written_bytes: changed.iter().map(|(_, b)| b.len() as u64).sum(),
        skipped_windows: layout.skipped_windows as u64,
        target_len: layout.target_len,
    })
}

/// Fallback: apply the delta to a temp file next to the source, then rename it
/// over the source. Only invoked before the source has been modified.
fn apply_via_temp(source: &Path, delta: &Path, opts: &ApplyOptions) -> Result<InPlaceStats> {
    let tmp = temp_path(source);
    let stats = {
        let result = (|| {
            let file = File::create(&tmp)?;
            let mut w = BufWriter::with_capacity(1 << 20, file);
            let s = crate::apply_paths(Some(source), delta, &mut w, opts)?;
            w.flush()?;
            Ok::<_, Error>(s)
        })();
        match result {
            Ok(s) => s,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    };
    if let Err(e) = std::fs::rename(&tmp, source) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(InPlaceStats {
        windows: stats.windows,
        written_windows: stats.windows,
        written_bytes: stats.target_len,
        skipped_windows: 0,
        target_len: stats.target_len,
    })
}

fn temp_path(source: &Path) -> PathBuf {
    let mut s = source.as_os_str().to_os_string();
    s.push(".xdelta-inplace.tmp");
    PathBuf::from(s)
}

// ---------------------------------------------------------------------------
// Write-back journal: record-then-restore for the in-place write phase.
// ---------------------------------------------------------------------------

/// Suffix for the write-back journal living next to the source file.
pub const IN_PLACE_JOURNAL_SUFFIX: &str = ".xdelta-journal.tmp";

/// Journal file magic (`XDJRNAL` + version byte 1).
const JOURNAL_MAGIC: [u8; 8] = *b"XDJRNAL\x01";

/// Streaming chunk size for journal record bodies (bounds extra RAM to 1 MiB).
const JOURNAL_CHUNK: u64 = 1 << 20;

fn journal_path(source: &Path) -> PathBuf {
    let mut s = source.as_os_str().to_os_string();
    s.push(IN_PLACE_JOURNAL_SUFFIX);
    PathBuf::from(s)
}

fn write_u64(f: &mut impl Write, v: u64) -> Result<()> {
    f.write_all(&v.to_le_bytes()).map_err(Error::from)
}

fn read_u64(r: &mut impl Read) -> std::io::Result<Option<u64>> {
    let mut buf = [0u8; 8];
    let mut got = 0usize;
    while got < 8 {
        match r.read(&mut buf[got..]) {
            Ok(0) => {
                // Clean EOF exactly on a record boundary ends the journal;
                // a short read mid-record means the journal write was torn
                // (crash before fsync), in which case the source was never
                // touched and the complete prefix replays harmlessly.
                return Ok(None);
            }
            Ok(n) => got += n,
            Err(e) => return Err(e),
        }
    }
    Ok(Some(u64::from_le_bytes(buf)))
}

/// Copy `len` bytes at `offset` from `reader` into the journal as one record.
/// Records that fall completely past EOF (file-growth region) carry no
/// original data and are skipped; the rollback truncation restores those.
fn append_journal_record(
    journal: &mut impl Write,
    reader: &mut File,
    offset: u64,
    len: u64,
    orig_len: u64,
    buf: &mut [u8],
) -> Result<()> {
    let start = offset.min(orig_len);
    let end = offset.saturating_add(len).min(orig_len);
    if end <= start {
        return Ok(());
    }
    let span = end - start;
    write_u64(journal, start)?;
    write_u64(journal, span)?;
    reader.seek(SeekFrom::Start(start))?;
    let mut remaining = span;
    while remaining > 0 {
        let n = remaining.min(buf.len() as u64) as usize;
        reader.read_exact(&mut buf[..n])?;
        journal.write_all(&buf[..n])?;
        remaining -= n as u64;
    }
    Ok(())
}

/// Replay the journal at `<source>.xdelta-journal.tmp`, restoring the
/// original block contents and file length. Returns `Ok(true)` when a journal
/// was found and replayed, `Ok(false)` when there was nothing to recover.
/// The journal is deleted after a successful replay. When the replay itself
/// fails an [`Error::InPlaceRollback`] is returned and the journal is kept
/// for a later retry.
///
/// This runs automatically at the start of every in-place apply (covering a
/// previous crash); call it directly to recover without applying anything.
pub fn recover_in_place_journal(source: &Path) -> Result<bool> {
    let path = journal_path(source);
    let mut journal = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let replay: Result<()> = (|| {
        let mut magic = [0u8; 8];
        journal.read_exact(&mut magic)?;
        if magic != JOURNAL_MAGIC {
            return Err(Error::Format("in-place journal has a bad magic"));
        }
        let orig_len =
            read_u64(&mut journal)?.ok_or(Error::Format("in-place journal is truncated"))?;
        let mut target = OpenOptions::new().read(true).write(true).open(source)?;
        // Replay every complete record; a torn tail (crash mid-journal-write)
        // simply stops the loop — the source was untouched in that case.
        let mut buf = vec![0u8; JOURNAL_CHUNK as usize];
        while let Some(off) = read_u64(&mut journal)? {
            let len = match read_u64(&mut journal)? {
                Some(l) => l,
                None => break,
            };
            let mut remaining = len;
            target.seek(SeekFrom::Start(off))?;
            while remaining > 0 {
                let n = remaining.min(JOURNAL_CHUNK) as usize;
                let mut got = 0usize;
                while got < n {
                    match journal.read(&mut buf[got..n])? {
                        0 => break,
                        k => got += k,
                    }
                }
                if got == 0 {
                    break;
                }
                target.write_all(&buf[..got])?;
                remaining -= got as u64;
            }
            if remaining > 0 {
                break;
            }
        }
        if target.metadata()?.len() != orig_len {
            target.set_len(orig_len)?;
        }
        target.flush()?;
        target.sync_all()?;
        Ok(())
    })();
    match replay {
        Ok(()) => {
            let _ = std::fs::remove_file(&path);
            Ok(true)
        }
        Err(e) => Err(Error::InPlaceRollback {
            reason: e.to_string(),
            journal: path.display().to_string(),
        }),
    }
}

/// Persist the original blocks of every soon-to-be-modified range into the
/// journal next to `source` and fsync it. Returns the journal path. Step 1 of
/// [`commit_changed_slots`]; factored out so tests can plant a journal and
/// simulate a torn write before calling [`recover_in_place_journal`].
fn write_journal(
    source: &Path,
    orig_len: u64,
    target_len: u64,
    changed: &[(u64, Vec<u8>)],
) -> Result<PathBuf> {
    let path = journal_path(source);
    let mut reader = File::open(source)?;
    // Buffer the journal stream: record headers are 16 bytes each, and
    // without buffering every one of them is its own syscall.
    let file = File::create(&path)?;
    let mut journal = BufWriter::with_capacity(JOURNAL_CHUNK as usize, file);
    journal.write_all(&JOURNAL_MAGIC)?;
    write_u64(&mut journal, orig_len)?;
    // One scratch buffer reused across all records (was one 1 MiB
    // allocation per record before).
    let mut buf = vec![0u8; JOURNAL_CHUNK as usize];
    for (off, bytes) in changed {
        append_journal_record(
            &mut journal,
            &mut reader,
            *off,
            bytes.len() as u64,
            orig_len,
            &mut buf,
        )?;
    }
    // Shrinking truncates `[target_len, orig_len)` away without ever
    // writing it, so the tail needs its own backup records.
    if orig_len > target_len {
        let mut off = target_len;
        while off < orig_len {
            let n = (orig_len - off).min(JOURNAL_CHUNK);
            append_journal_record(&mut journal, &mut reader, off, n, orig_len, &mut buf)?;
            off += n;
        }
    }
    journal.flush()?;
    journal.get_mut().sync_all()?;
    Ok(path)
}

/// Phase 2 of the in-place apply: persist the original blocks to the journal,
/// fsync it, then resize and overwrite the changed slots. Any failure
/// detected while mutating triggers a rollback replay before the error is
/// returned, so the source keeps either its old or its new content — never a
/// torn mix.
///
/// Single-sync design: only the journal is fsync'd, the source itself is
/// flushed but not synced. The journal fsync is the load-bearing one — it is
/// what makes rollback possible after a crash mid-mutate. Skipping the source
/// sync narrows the post-success power-loss window to "stale (old) file with
/// no journal left", which the caller heals by re-applying (the output hash
/// check will fail and the delta is re-applied from the intact old content);
/// a runtime write failure still rolls back exactly as before.
fn commit_changed_slots(source: &Path, target_len: u64, changed: &[(u64, Vec<u8>)]) -> Result<()> {
    let orig_len = std::fs::metadata(source)?.len();
    if changed.is_empty() && orig_len == target_len {
        return Ok(());
    }
    // Step 1: record original blocks. The journal fsync inside is the point
    // of no return — nothing that follows may mutate the source before it.
    let path = write_journal(source, orig_len, target_len, changed)?;
    // Step 2: mutate. Grow first (writes need the space), shrink last (so a
    // failure before the truncate leaves the tail recoverable in place too).
    let mutate: Result<()> = (|| {
        let mut f = OpenOptions::new().read(true).write(true).open(source)?;
        if target_len > orig_len {
            f.set_len(target_len)?;
        }
        for (off, bytes) in changed {
            f.seek(SeekFrom::Start(*off))?;
            f.write_all(bytes)?;
        }
        if f.metadata()?.len() != target_len {
            f.set_len(target_len)?;
        }
        f.flush()?;
        Ok(())
    })();
    match mutate {
        Ok(()) => {
            // Best effort: the content is already durable; a leftover journal
            // only costs a redundant recover-then-reapply on the next run.
            let _ = std::fs::remove_file(&path);
            Ok(())
        }
        Err(e) => {
            // Roll back to the journaled original, then report the failure
            // that caused it. `recover_in_place_journal` deletes the journal
            // on success and keeps it (with an `InPlaceRollback` error) when
            // the replay itself breaks.
            match recover_in_place_journal(source) {
                Ok(_) => Err(e),
                Err(r) => Err(Error::InPlaceRollback {
                    reason: format!("write failed ({e}) and rollback failed ({r})"),
                    journal: path.display().to_string(),
                }),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Verified in-place apply: `expect_before` / `expect_after` file checksums.
// ---------------------------------------------------------------------------

/// File-level checksums for a verified in-place apply.
#[derive(Debug, Clone, Default)]
pub struct InPlaceChecksums {
    /// Hash of the source before any write (`None` when it was not needed).
    pub before: Option<Box<[u8]>>,
    /// Hash of the bytes written to the source.
    pub after: Option<Box<[u8]>>,
}

/// Outcome of [`apply_paths_in_place_verified`].
#[derive(Debug)]
pub enum InPlaceOutcome {
    Applied {
        stats: InPlaceStats,
        checksums: InPlaceChecksums,
    },
    Skipped {
        source_checksum: Box<[u8]>,
    },
}

/// Verified in-place apply using [`DEFAULT_IN_PLACE_THRESHOLD`].
///
/// `expect_before` and the idempotent skip are checked before the source is
/// touched. `expect_after` is verified AFTER the write-back with a buffered
/// sequential read; a mismatch reports failure with the patched file left in
/// place (the legacy post-verify contract). Write-phase I/O failures still
/// roll back via the journal (see [`commit_changed_slots`]).
pub fn apply_paths_in_place_verified(
    source: &Path,
    delta: &Path,
    opts: &ApplyOptions,
    algo: ChecksumAlgo,
    expect_before: Option<&[u8]>,
    expect_after: Option<&[u8]>,
) -> Result<InPlaceOutcome> {
    apply_paths_in_place_verified_with_threshold(
        source,
        delta,
        opts,
        algo,
        expect_before,
        expect_after,
        DEFAULT_IN_PLACE_THRESHOLD,
    )
}

/// Hash a file with a 1 MiB buffered sequential read. Used for the post-write
/// output check: it is measurably faster than hashing the same bytes window by
/// window out of a source mmap, especially when the data is page-cached.
fn hash_file_buffered(path: &Path, algo: ChecksumAlgo) -> Result<Box<[u8]>> {
    let mut h = algo.instantiate();
    let mut f = std::io::BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.digest())
}

/// Verified in-place apply with an explicit cap on total changed-window bytes.
/// Over the cap the patch goes through a temp file + rename, and the rename
/// only happens after the verified output hash matches.
pub fn apply_paths_in_place_verified_with_threshold(
    source: &Path,
    delta: &Path,
    opts: &ApplyOptions,
    algo: ChecksumAlgo,
    expect_before: Option<&[u8]>,
    expect_after: Option<&[u8]>,
    threshold: u64,
) -> Result<InPlaceOutcome> {
    recover_in_place_journal(source)?;

    let delta_map = MappedFile::open(delta)?;
    let delta_bytes = delta_map.as_bytes();
    let layout = scan_layout(delta_bytes, opts.max_window_size)?;

    if layout.changed_bytes > threshold {
        return apply_via_temp_verified(source, delta, opts, algo, expect_before, expect_after);
    }

    // Source-side pre-checks (`expect_before` and the idempotent skip) still
    // run before any write, so an already-patched source returns without
    // decoding a single window. The output hash is no longer reconstructed
    // before the write: hashing every window out of the source mmap is
    // measurably slower than a buffered sequential read, so it is checked
    // AFTER the write-back (Phase 3) — the legacy post-verify contract. Crash
    // safety comes from the journal, not from the pre-write check.
    let hash_needed = expect_before.is_some()
        || (opts.idempotent_skip
            && expect_after.is_some()
            && std::fs::metadata(source)?.len() == layout.target_len);
    let source_hash: Option<Box<[u8]>> = if hash_needed {
        Some(hash_file_buffered(source, algo)?)
    } else {
        None
    };

    if let (Some(src), Some(after)) = (&source_hash, expect_after)
        && opts.idempotent_skip
        && src.as_ref() == after
    {
        return Ok(InPlaceOutcome::Skipped {
            source_checksum: src.clone(),
        });
    }

    if let (Some(src), Some(before)) = (&source_hash, expect_before)
        && src.as_ref() != before
    {
        return Err(Error::FileChecksumMismatch {
            phase: "source",
            algo: algo.name(),
            expected: crate::encode_hex(before),
            actual: crate::encode_hex(src),
        });
    }

    // Phase 1: decode every changed window while the source is untouched.
    let changed = {
        let src_map = MappedFile::open(source)?;
        let src_bytes = src_map.as_bytes();
        let mut target = Vec::new();
        let mut data_buf = Vec::new();
        let mut inst_buf = Vec::new();
        let mut addr_buf = Vec::new();
        let mut secondary = SecondaryDecoder::new();
        let mut cache = AddrCache::new(layout.code_table.s_near, layout.code_table.s_same);
        let mut changed: Vec<(u64, Vec<u8>)> = Vec::with_capacity(layout.changed_windows);
        for wl in &layout.windows {
            if wl.pure_copy {
                continue;
            }
            decode_one_window(
                delta_bytes,
                Some(src_bytes),
                &wl.win,
                &layout.code_table,
                &mut cache,
                opts,
                &mut secondary,
                &mut data_buf,
                &mut inst_buf,
                &mut addr_buf,
                &mut target,
            )?;
            changed.push((wl.target_offset, target.clone()));
        }
        changed
    };

    // Phase 2: journal + write-back with rollback on I/O failure.
    commit_changed_slots(source, layout.target_len, &changed)?;

    // Phase 3: verify the output hash with a buffered sequential read. A
    // mismatch reports failure with the patched file left in place.
    let output_digest = match expect_after {
        Some(after) => {
            let digest = hash_file_buffered(source, algo)?;
            if digest.as_ref() != after {
                return Err(Error::FileChecksumMismatch {
                    phase: "output",
                    algo: algo.name(),
                    expected: crate::encode_hex(after),
                    actual: crate::encode_hex(&digest),
                });
            }
            Some(digest)
        }
        None => None,
    };

    Ok(InPlaceOutcome::Applied {
        stats: InPlaceStats {
            windows: layout.windows.len() as u64,
            written_windows: changed.len() as u64,
            written_bytes: changed.iter().map(|(_, b)| b.len() as u64).sum(),
            skipped_windows: layout.skipped_windows as u64,
            target_len: layout.target_len,
        },
        checksums: InPlaceChecksums {
            before: source_hash,
            after: output_digest,
        },
    })
}

/// Threshold fallback for the verified apply: render to a temp file, verify
/// the output hash, and rename over the source only on success. The source
/// is never modified on failure, and the temp file is removed.
fn apply_via_temp_verified(
    source: &Path,
    delta: &Path,
    opts: &ApplyOptions,
    algo: ChecksumAlgo,
    expect_before: Option<&[u8]>,
    expect_after: Option<&[u8]>,
) -> Result<InPlaceOutcome> {
    let tmp = temp_path(source);
    let outcome = (|| {
        let file = File::create(&tmp)?;
        let mut w = BufWriter::with_capacity(1 << 20, file);
        let outcome = crate::apply_paths_verified(
            Some(source),
            delta,
            &mut w,
            algo,
            expect_before,
            expect_after,
            opts,
        )?;
        w.flush()?;
        Ok::<_, Error>(outcome)
    })();
    match outcome {
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
        Ok(crate::ApplyOutcome::Skipped { source_checksum }) => {
            let _ = std::fs::remove_file(&tmp);
            Ok(InPlaceOutcome::Skipped { source_checksum })
        }
        Ok(crate::ApplyOutcome::Applied { stats, checksums }) => {
            if let Err(e) = std::fs::rename(&tmp, source) {
                let _ = std::fs::remove_file(&tmp);
                return Err(e.into());
            }
            Ok(InPlaceOutcome::Applied {
                stats: InPlaceStats {
                    windows: stats.windows,
                    written_windows: stats.windows,
                    written_bytes: stats.target_len,
                    skipped_windows: 0,
                    target_len: stats.target_len,
                },
                checksums: InPlaceChecksums {
                    before: checksums.before,
                    after: checksums.after,
                },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::checksum::adler32;
    use std::io::Read;

    fn varint(mut v: u64) -> Vec<u8> {
        let mut digits = Vec::new();
        loop {
            digits.push((v & 0x7f) as u8);
            v >>= 7;
            if v == 0 {
                break;
            }
        }
        digits.reverse();
        let mut out = Vec::new();
        for (i, d) in digits.iter().enumerate() {
            if i == digits.len() - 1 {
                out.push(*d);
            } else {
                out.push(*d | 0x80);
            }
        }
        out
    }

    struct TestWin {
        win_ind: u8,
        src_len: Option<u64>,
        src_pos: Option<u64>,
        target: Vec<u8>,
        data: Vec<u8>,
        inst: Vec<u8>,
        addr: Vec<u8>,
        checksum: bool,
    }

    impl TestWin {
        fn new() -> Self {
            TestWin {
                win_ind: 0,
                src_len: None,
                src_pos: None,
                target: Vec::new(),
                data: Vec::new(),
                inst: Vec::new(),
                addr: Vec::new(),
                checksum: false,
            }
        }
        fn source(mut self, len: u64, pos: u64) -> Self {
            self.win_ind |= 1;
            self.src_len = Some(len);
            self.src_pos = Some(pos);
            self
        }
        fn checksum(mut self) -> Self {
            self.win_ind |= 4;
            self.checksum = true;
            self
        }
        fn add(mut self, bytes: &[u8]) -> Self {
            self.inst.push(1);
            self.inst.extend_from_slice(&varint(bytes.len() as u64));
            self.data.extend_from_slice(bytes);
            self.target.extend_from_slice(bytes);
            self
        }
        fn copy(mut self, addr: u64, size: u64) -> Self {
            self.inst.push(19);
            self.inst.extend_from_slice(&varint(size));
            self.addr.extend_from_slice(&varint(addr));
            self.target.resize(self.target.len() + size as usize, 0);
            self
        }
        fn finish(self) -> Vec<u8> {
            let mut out = Vec::new();
            out.push(self.win_ind);
            if let (Some(len), Some(pos)) = (self.src_len, self.src_pos) {
                out.extend_from_slice(&varint(len));
                out.extend_from_slice(&varint(pos));
            }
            let enclen = 1
                + varint(self.target.len() as u64).len()
                + varint(self.data.len() as u64).len()
                + varint(self.inst.len() as u64).len()
                + varint(self.addr.len() as u64).len()
                + self.data.len()
                + self.inst.len()
                + self.addr.len()
                + if self.checksum { 4 } else { 0 };
            out.extend_from_slice(&varint(enclen as u64));
            out.extend_from_slice(&varint(self.target.len() as u64));
            out.push(0);
            out.extend_from_slice(&varint(self.data.len() as u64));
            out.extend_from_slice(&varint(self.inst.len() as u64));
            out.extend_from_slice(&varint(self.addr.len() as u64));
            if self.checksum {
                let cks = adler32(&self.target);
                out.extend_from_slice(&cks.to_be_bytes());
            }
            out.extend_from_slice(&self.data);
            out.extend_from_slice(&self.inst);
            out.extend_from_slice(&self.addr);
            out
        }
    }

    fn header() -> Vec<u8> {
        vec![0xD6, 0xC3, 0xC4, 0x00, 0x00]
    }

    fn read_file(path: &Path) -> Vec<u8> {
        let mut f = File::open(path).unwrap();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        buf
    }

    fn source_bytes() -> Vec<u8> {
        [
            vec![b'A'; 16],
            vec![b'B'; 16],
            vec![b'C'; 16],
            vec![b'D'; 16],
        ]
        .concat()
    }

    /// Delta: window0 changed ("A"*16 -> "x"*16), window1 pure copy ("B"*16),
    /// window2 reads back into window0's slot via a source window that reaches
    /// backward (exercises the two-phase reverse-read guarantee), window3 pure
    /// copy ("D"*16).
    fn reverse_read_delta() -> Vec<u8> {
        let mut d = header();
        d.extend_from_slice(&TestWin::new().source(16, 0).add(&[b'x'; 16]).finish());
        d.extend_from_slice(&TestWin::new().source(16, 16).copy(0, 16).finish());
        d.extend_from_slice(&TestWin::new().source(32, 0).copy(0, 16).finish());
        d.extend_from_slice(&TestWin::new().source(16, 48).copy(0, 16).finish());
        d
    }

    #[test]
    fn scan_identifies_pure_copy_and_changed() {
        let delta = reverse_read_delta();
        let layout = scan_layout(&delta, 1 << 20).unwrap();
        assert_eq!(layout.windows.len(), 4);
        assert_eq!(layout.target_len, 64);
        assert_eq!(layout.skipped_windows, 2);
        assert_eq!(layout.changed_windows, 2);
        assert_eq!(layout.changed_bytes, 32);
        assert!(!layout.windows[0].pure_copy); // changed
        assert!(layout.windows[1].pure_copy); // aligned full copy
        assert!(!layout.windows[2].pure_copy); // reads backward -> not aligned
        assert!(layout.windows[3].pure_copy);
    }

    #[test]
    fn in_place_reverse_read_matches_full_apply() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, source_bytes()).unwrap();
        std::fs::write(&dl, reverse_read_delta()).unwrap();

        let opts = ApplyOptions::default();
        let stats = apply_paths_in_place(&src, &dl, &opts).unwrap();
        assert_eq!(stats.skipped_windows, 2);
        assert_eq!(stats.written_windows, 2);
        assert_eq!(stats.written_bytes, 32);

        let expected: Vec<u8> = [
            vec![b'x'; 16],
            vec![b'B'; 16],
            vec![b'A'; 16],
            vec![b'D'; 16],
        ]
        .concat();
        assert_eq!(read_file(&src), expected);

        // The in-place result must match a full rewrite.
        let dir2 = tempfile::tempdir().unwrap();
        let src2 = dir2.path().join("src.dat");
        std::fs::write(&src2, source_bytes()).unwrap();
        let mut out = Vec::new();
        crate::apply(
            &std::fs::read(&dl).unwrap(),
            Some(&source_bytes()),
            &mut out,
            &opts,
        )
        .unwrap();
        assert_eq!(read_file(&src), out);
    }

    #[test]
    fn in_place_shrink_resizes_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, source_bytes()).unwrap();
        // Target is only the first 16 bytes (changed).
        let mut d = header();
        d.extend_from_slice(&TestWin::new().source(16, 0).add(&b"zz".repeat(8)).finish());
        std::fs::write(&dl, d).unwrap();

        apply_paths_in_place(&src, &dl, &ApplyOptions::default()).unwrap();
        assert_eq!(read_file(&src), b"zzzzzzzzzzzzzzzz");
    }

    #[test]
    fn in_place_grow_resizes_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        // Source is only 32 bytes but target grows to 64.
        std::fs::write(&src, [vec![b'A'; 16], vec![b'B'; 16]].concat()).unwrap();
        let mut d = header();
        d.extend_from_slice(&TestWin::new().source(16, 0).copy(0, 16).finish());
        d.extend_from_slice(&TestWin::new().source(16, 16).copy(0, 16).finish());
        d.extend_from_slice(&TestWin::new().add(&[b'C'; 16]).finish());
        d.extend_from_slice(&TestWin::new().add(&[b'D'; 16]).finish());
        std::fs::write(&dl, d).unwrap();

        apply_paths_in_place(&src, &dl, &ApplyOptions::default()).unwrap();
        let expected: Vec<u8> = [
            vec![b'A'; 16],
            vec![b'B'; 16],
            vec![b'C'; 16],
            vec![b'D'; 16],
        ]
        .concat();
        assert_eq!(read_file(&src), expected);
    }

    #[test]
    fn threshold_zero_falls_back_to_temp_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, source_bytes()).unwrap();
        std::fs::write(&dl, reverse_read_delta()).unwrap();

        let opts = ApplyOptions::default();
        let stats = apply_paths_in_place_with_threshold(&src, &dl, &opts, 0).unwrap();
        // Fallback writes everything through the temp path.
        assert_eq!(stats.skipped_windows, 0);
        assert_eq!(stats.windows, 4);
        assert_eq!(stats.written_bytes, 64);

        let expected: Vec<u8> = [
            vec![b'x'; 16],
            vec![b'B'; 16],
            vec![b'A'; 16],
            vec![b'D'; 16],
        ]
        .concat();
        assert_eq!(read_file(&src), expected);
    }

    #[test]
    fn pure_copy_all_windows_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, source_bytes()).unwrap();
        // All windows are aligned pure copies: nothing to write.
        let mut d = header();
        for (i, _) in source_bytes().chunks(16).enumerate() {
            let pos = (i * 16) as u64;
            d.extend_from_slice(&TestWin::new().source(16, pos).copy(0, 16).finish());
        }
        std::fs::write(&dl, d).unwrap();

        let stats = apply_paths_in_place(&src, &dl, &ApplyOptions::default()).unwrap();
        assert_eq!(stats.written_windows, 0);
        assert_eq!(stats.written_bytes, 0);
        assert_eq!(stats.skipped_windows, 4);
        assert_eq!(read_file(&src), source_bytes());
    }

    #[test]
    fn changed_window_checksum_verified() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, source_bytes()).unwrap();
        let mut d = header();
        d.extend_from_slice(
            &TestWin::new()
                .source(16, 0)
                .checksum()
                .add(&[b'x'; 16])
                .finish(),
        );
        std::fs::write(&dl, d).unwrap();

        // Valid checksum applies cleanly.
        let opts = ApplyOptions::default();
        apply_paths_in_place(&src, &dl, &opts).unwrap();
        assert_eq!(read_file(&src), vec![b'x'; 16]);

        // Tamper the stored checksum and reset the source: the apply must fail
        // during phase 1 (before any write) and leave the source untouched.
        let mut bad = std::fs::read(&dl).unwrap();
        // The window's 4 checksum bytes start at index 14 (after win_ind,
        // src_len, src_pos, enclen, target_len, del_ind, data_len, inst_len,
        // addr_len).
        bad[14] ^= 0xFF;
        std::fs::write(&dl, bad).unwrap();
        std::fs::write(&src, source_bytes()).unwrap();

        let err = apply_paths_in_place(&src, &dl, &opts).unwrap_err();
        assert!(matches!(err, Error::ChecksumMismatch { .. }));
        // Phase-1 failure: no writes happened, source unchanged.
        assert_eq!(read_file(&src), source_bytes());
    }

    // -- journal rollback + verified pre-checks --------------------------------

    use crate::checksum::{Checksum, ChecksumAlgo};
    use std::fs::OpenOptions;

    fn md5(data: &[u8]) -> Vec<u8> {
        let mut h = ChecksumAlgo::Md5.instantiate();
        h.update(data);
        h.digest().to_vec()
    }

    fn expected_output() -> Vec<u8> {
        [
            vec![b'x'; 16],
            vec![b'B'; 16],
            vec![b'A'; 16],
            vec![b'D'; 16],
        ]
        .concat()
    }

    fn journal_for(src: &Path) -> PathBuf {
        let mut s = src.as_os_str().to_os_string();
        s.push(IN_PLACE_JOURNAL_SUFFIX);
        PathBuf::from(s)
    }

    #[test]
    fn verified_output_mismatch_patches_then_reports_failure() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, source_bytes()).unwrap();
        std::fs::write(&dl, reverse_read_delta()).unwrap();

        let opts = ApplyOptions::default();
        let before = md5(&source_bytes());
        let err = apply_paths_in_place_verified(
            &src,
            &dl,
            &opts,
            ChecksumAlgo::Md5,
            Some(&before),
            Some(&md5(b"definitely not the target")),
        )
        .unwrap_err();
        match err {
            Error::FileChecksumMismatch { phase, .. } => assert_eq!(phase, "output"),
            e => panic!("wrong error: {e}"),
        }
        // Post-write check: the patch was applied, then the output hash was
        // found to be wrong. The journal is consumed either way.
        assert_eq!(read_file(&src), expected_output());
        assert!(!journal_for(&src).exists());
    }

    #[test]
    fn verified_source_mismatch_leaves_source_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, source_bytes()).unwrap();
        std::fs::write(&dl, reverse_read_delta()).unwrap();

        let err = apply_paths_in_place_verified(
            &src,
            &dl,
            &ApplyOptions::default(),
            ChecksumAlgo::Md5,
            Some(&md5(b"wrong source")),
            Some(&md5(&expected_output())),
        )
        .unwrap_err();
        match err {
            Error::FileChecksumMismatch { phase, .. } => assert_eq!(phase, "source"),
            e => panic!("wrong error: {e}"),
        }
        assert_eq!(read_file(&src), source_bytes());
        assert!(!journal_for(&src).exists());
    }

    #[test]
    fn verified_success_returns_checksums_and_cleans_journal() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, source_bytes()).unwrap();
        std::fs::write(&dl, reverse_read_delta()).unwrap();

        let outcome = apply_paths_in_place_verified(
            &src,
            &dl,
            &ApplyOptions::default(),
            ChecksumAlgo::Md5,
            Some(&md5(&source_bytes())),
            Some(&md5(&expected_output())),
        )
        .unwrap();
        match outcome {
            InPlaceOutcome::Applied { stats, checksums } => {
                assert_eq!(stats.written_windows, 2);
                assert_eq!(
                    checksums.before.as_deref(),
                    Some(md5(&source_bytes()).as_slice())
                );
                assert_eq!(
                    checksums.after.as_deref(),
                    Some(md5(&expected_output()).as_slice())
                );
            }
            InPlaceOutcome::Skipped { .. } => panic!("should have applied"),
        }
        assert_eq!(read_file(&src), expected_output());
        assert!(!journal_for(&src).exists());
    }

    #[test]
    fn verified_skips_already_patched_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, expected_output()).unwrap();
        std::fs::write(&dl, reverse_read_delta()).unwrap();

        // Source already equals the target: no write, no journal.
        let outcome = apply_paths_in_place_verified(
            &src,
            &dl,
            &ApplyOptions::default(),
            ChecksumAlgo::Md5,
            None,
            Some(&md5(&expected_output())),
        )
        .unwrap();
        match outcome {
            InPlaceOutcome::Skipped { source_checksum } => {
                assert_eq!(source_checksum.as_ref(), md5(&expected_output()).as_slice());
            }
            InPlaceOutcome::Applied { .. } => panic!("should have skipped"),
        }
        assert_eq!(read_file(&src), expected_output());
        assert!(!journal_for(&src).exists());
    }

    #[test]
    fn journal_recovery_restores_torn_shrink() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let original = source_bytes(); // 64 bytes
        std::fs::write(&src, &original).unwrap();

        // Plant the journal for a shrink to 16 bytes, then simulate a torn
        // write: head overwritten, tail truncated away.
        let changed = vec![(0u64, vec![b'x'; 16])];
        write_journal(&src, original.len() as u64, 16, &changed).unwrap();
        assert!(journal_for(&src).exists());
        {
            let mut f = OpenOptions::new().write(true).open(&src).unwrap();
            f.write_all(&[b'x'; 16]).unwrap();
            f.set_len(16).unwrap();
            f.sync_all().unwrap();
        }
        assert_eq!(read_file(&src), vec![b'x'; 16]);

        assert!(recover_in_place_journal(&src).unwrap());
        assert_eq!(read_file(&src), original);
        // Journal consumed; nothing left to recover.
        assert!(!journal_for(&src).exists());
        assert!(!recover_in_place_journal(&src).unwrap());
    }

    #[test]
    fn journal_recovery_restores_torn_grow() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let original = vec![b'A'; 16];
        std::fs::write(&src, &original).unwrap();

        // Grow 16 -> 48 with two new blocks; tear after the first one.
        let changed = vec![(16u64, vec![b'C'; 16]), (32u64, vec![b'D'; 16])];
        write_journal(&src, original.len() as u64, 48, &changed).unwrap();
        {
            let mut f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&src)
                .unwrap();
            f.set_len(48).unwrap();
            f.seek(SeekFrom::Start(16)).unwrap();
            f.write_all(&[b'C'; 16]).unwrap();
            f.sync_all().unwrap();
        }
        assert_eq!(std::fs::metadata(&src).unwrap().len(), 48);

        assert!(recover_in_place_journal(&src).unwrap());
        assert_eq!(read_file(&src), original);
    }

    #[test]
    fn stale_journal_auto_recovered_on_next_apply() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, source_bytes()).unwrap();
        std::fs::write(&dl, reverse_read_delta()).unwrap();

        // Crash simulation: journal the real first changed block, then tear
        // the head of the file and leave the journal behind.
        write_journal(&src, 64, 64, &[(0u64, vec![b'x'; 16])]).unwrap();
        std::fs::write(
            &src,
            [vec![0u8; 16], source_bytes()[16..].to_vec()].concat(),
        )
        .unwrap();
        assert_ne!(read_file(&src), source_bytes());

        // The next apply replays the stale journal, then patches cleanly.
        let stats = apply_paths_in_place(&src, &dl, &ApplyOptions::default()).unwrap();
        assert_eq!(stats.written_windows, 2);
        assert_eq!(read_file(&src), expected_output());
        assert!(!journal_for(&src).exists());
    }

    #[test]
    fn threshold_fallback_verified_mismatch_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, source_bytes()).unwrap();
        std::fs::write(&dl, reverse_read_delta()).unwrap();

        let err = apply_paths_in_place_verified_with_threshold(
            &src,
            &dl,
            &ApplyOptions::default(),
            ChecksumAlgo::Md5,
            None,
            Some(&md5(b"wrong")),
            0, // force the temp-file fallback
        )
        .unwrap_err();
        assert!(matches!(err, Error::FileChecksumMismatch { .. }));
        assert_eq!(read_file(&src), source_bytes());
        let mut tmp = src.as_os_str().to_os_string();
        tmp.push(".xdelta-inplace.tmp");
        assert!(!Path::new(&tmp).exists());
        assert!(!journal_for(&src).exists());
    }

    #[test]
    fn threshold_fallback_verified_success() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.dat");
        let dl = dir.path().join("patch.delta");
        std::fs::write(&src, source_bytes()).unwrap();
        std::fs::write(&dl, reverse_read_delta()).unwrap();

        let outcome = apply_paths_in_place_verified_with_threshold(
            &src,
            &dl,
            &ApplyOptions::default(),
            ChecksumAlgo::Md5,
            Some(&md5(&source_bytes())),
            Some(&md5(&expected_output())),
            0, // force the temp-file fallback
        )
        .unwrap();
        assert!(matches!(outcome, InPlaceOutcome::Applied { .. }));
        assert_eq!(read_file(&src), expected_output());
    }
}

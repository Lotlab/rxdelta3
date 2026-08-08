//! In-place patch application: rewrite only the window slots whose content
//! actually changed, instead of writing the whole output file.
//!
//! The source file is patched in place (output == source). Correctness relies
//! on a two-phase strategy: every window is decoded while the source is still
//! untouched, and only then are the changed slots written back. This avoids
//! the cross-window read-after-write hazard where a window's COPY reads bytes
//! that an earlier window's write already overwrote.
//!
//! Window decoding is shared with `decode_one_window`; this
//! module only adds the layout scan, pure-copy detection, two-phase write-back
//! and the temp-rename threshold fallback.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::address::AddrCache;
use super::code_table::{CodeTable, COPY, NOOP};
use super::decode_one_window;
use super::header::{parse_header, SecondaryId};
use super::secondary::SecondaryDecoder;
use super::window::{parse_window, Window, VCD_SOURCE};
use crate::errors::{Error, Result};
use crate::io::MappedFile;
use crate::varint::read_usize;
use crate::ApplyOptions;

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
    let Some(&op) = delta.get(w.inst_start..w.addr_start).and_then(|s| s.first()) else {
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
pub fn apply_paths_in_place(source: &Path, delta: &Path, opts: &ApplyOptions) -> Result<InPlaceStats> {
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

    // Phase 2: resize, then write the changed slots through a writable handle.
    let mut src_file = OpenOptions::new().read(true).write(true).open(source)?;
    if src_file.metadata()?.len() != layout.target_len {
        src_file.set_len(layout.target_len)?;
    }
    for (off, bytes) in &changed {
        src_file.seek(SeekFrom::Start(*off))?;
        src_file.write_all(bytes)?;
    }
    src_file.flush()?;

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
        let file = File::create(&tmp)?;
        let mut w = BufWriter::with_capacity(1 << 20, file);
        let s = crate::apply_paths(Some(source), delta, &mut w, opts)?;
        w.flush()?;
        s
    };
    std::fs::rename(&tmp, source)?;
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
        let expected: Vec<u8> = [vec![b'A'; 16], vec![b'B'; 16], vec![b'C'; 16], vec![b'D'; 16]]
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
}
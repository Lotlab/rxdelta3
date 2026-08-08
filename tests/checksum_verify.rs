use std::fs;
use std::io::Write;

use tempfile::tempdir;

use rxdelta::{
    ApplyOptions, ApplyOutcome, Checksum, ChecksumAlgo, Md5, apply, apply_paths,
    apply_paths_verified,
};

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

struct WindowBuilder {
    win_ind: u8,
    src_len: Option<u64>,
    src_pos: Option<u64>,
    target: Vec<u8>,
    data: Vec<u8>,
    inst: Vec<u8>,
    addr: Vec<u8>,
}

impl WindowBuilder {
    fn new() -> Self {
        WindowBuilder {
            win_ind: 0,
            src_len: None,
            src_pos: None,
            target: Vec::new(),
            data: Vec::new(),
            inst: Vec::new(),
            addr: Vec::new(),
        }
    }

    fn source(mut self, len: u64, pos: u64) -> Self {
        self.win_ind |= 1;
        self.src_len = Some(len);
        self.src_pos = Some(pos);
        self
    }

    fn add(mut self, bytes: &[u8]) -> Self {
        self.inst.extend_from_slice(&[1]);
        self.inst.extend_from_slice(&varint(bytes.len() as u64));
        self.data.extend_from_slice(bytes);
        self.target.extend_from_slice(bytes);
        self
    }

    fn copy(mut self, addr: u64, size: u64) -> Self {
        self.inst.extend_from_slice(&[19]);
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
            + self.addr.len();
        out.extend_from_slice(&varint(enclen as u64));
        out.extend_from_slice(&varint(self.target.len() as u64));
        out.push(0);
        out.extend_from_slice(&varint(self.data.len() as u64));
        out.extend_from_slice(&varint(self.inst.len() as u64));
        out.extend_from_slice(&varint(self.addr.len() as u64));
        out.extend_from_slice(&self.data);
        out.extend_from_slice(&self.inst);
        out.extend_from_slice(&self.addr);
        out
    }
}

fn delta_with_source() -> Vec<u8> {
    let mut d = vec![0xD6, 0xC3, 0xC4, 0x00, 0x00];
    d.extend_from_slice(
        &WindowBuilder::new()
            .source(8, 4)
            .add(b"XYZ")
            .copy(2, 6)
            .finish(),
    );
    d
}

fn delta_add_only_of(target: &[u8]) -> Vec<u8> {
    let mut d = vec![0xD6, 0xC3, 0xC4, 0x00, 0x00];
    d.extend_from_slice(&WindowBuilder::new().add(target).finish());
    d
}

fn delta_add_only() -> Vec<u8> {
    delta_add_only_of(b"hello")
}

const SRC: &[u8] = b"0123456789abcdef";
const TARGET: &[u8] = b"XYZ6789ab";

fn md5_bytes(data: &[u8]) -> Vec<u8> {
    let mut h = Md5::new();
    h.update(data);
    h.digest().to_vec()
}

fn opts() -> ApplyOptions {
    ApplyOptions::default()
}

#[test]
fn verified_apply_success() {
    let dir = tempdir().unwrap();
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&src_path, SRC).unwrap();
    fs::write(&delta_path, delta_with_source()).unwrap();

    let before = md5_bytes(SRC);
    let after = md5_bytes(TARGET);
    let mut out = Vec::new();
    let result = apply_paths_verified(
        Some(&src_path),
        &delta_path,
        &mut out,
        ChecksumAlgo::Md5,
        Some(&before),
        Some(&after),
        &opts(),
    )
    .unwrap();

    match result {
        ApplyOutcome::Applied { stats, checksums } => {
            assert_eq!(stats.target_len, TARGET.len() as u64);
            assert_eq!(out, TARGET);
            assert_eq!(checksums.before.unwrap().as_ref(), before);
            assert_eq!(checksums.after.unwrap().as_ref(), after);
        }
        ApplyOutcome::Skipped { .. } => panic!("expected applied"),
    }
}

#[test]
fn verified_skip_when_already_patched() {
    let dir = tempdir().unwrap();
    // source file already contains the patched content
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&src_path, TARGET).unwrap();
    fs::write(&delta_path, delta_with_source()).unwrap();

    let after = md5_bytes(TARGET);
    let mut out = Vec::new();
    let result = apply_paths_verified(
        Some(&src_path),
        &delta_path,
        &mut out,
        ChecksumAlgo::Md5,
        None,
        Some(&after),
        &opts(),
    )
    .unwrap();

    match result {
        ApplyOutcome::Skipped { source_checksum } => {
            assert_eq!(source_checksum.as_ref(), after);
        }
        ApplyOutcome::Applied { .. } => panic!("expected skipped"),
    }
    assert!(out.is_empty(), "skip must not write output");
}

#[test]
fn verified_skip_takes_priority_over_before_verify() {
    let dir = tempdir().unwrap();
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&src_path, TARGET).unwrap();
    fs::write(&delta_path, delta_with_source()).unwrap();

    let before = md5_bytes(SRC); // does not match the already-patched source
    let after = md5_bytes(TARGET);
    let mut out = Vec::new();
    let result = apply_paths_verified(
        Some(&src_path),
        &delta_path,
        &mut out,
        ChecksumAlgo::Md5,
        Some(&before),
        Some(&after),
        &opts(),
    )
    .unwrap();
    assert!(matches!(result, ApplyOutcome::Skipped { .. }));
}

#[test]
fn verified_source_mismatch_errors_without_output() {
    let dir = tempdir().unwrap();
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&src_path, b"zzzzzzzzzzzzzzzz").unwrap();
    fs::write(&delta_path, delta_with_source()).unwrap();

    let before = md5_bytes(SRC);
    let after = md5_bytes(TARGET);
    let mut out = Vec::new();
    let err = apply_paths_verified(
        Some(&src_path),
        &delta_path,
        &mut out,
        ChecksumAlgo::Md5,
        Some(&before),
        Some(&after),
        &opts(),
    )
    .unwrap_err();
    match &err {
        rxdelta::Error::FileChecksumMismatch { phase, .. } => assert_eq!(*phase, "source"),
        other => panic!("unexpected error: {other}"),
    }
    assert!(out.is_empty(), "source mismatch must not write output");
}

#[test]
fn verified_output_mismatch_errors_after_write() {
    let dir = tempdir().unwrap();
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&src_path, SRC).unwrap();
    fs::write(&delta_path, delta_with_source()).unwrap();

    let before = md5_bytes(SRC);
    let wrong_after = md5_bytes(b"different target");
    let mut out = Vec::new();
    let err = apply_paths_verified(
        Some(&src_path),
        &delta_path,
        &mut out,
        ChecksumAlgo::Md5,
        Some(&before),
        Some(&wrong_after),
        &opts(),
    )
    .unwrap_err();
    match &err {
        rxdelta::Error::FileChecksumMismatch { phase, .. } => assert_eq!(*phase, "output"),
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(out, TARGET, "output written before verification");
}

#[test]
fn verified_compression_only_with_output_verify() {
    let dir = tempdir().unwrap();
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&delta_path, delta_add_only()).unwrap();

    let after = {
        let mut h = rxdelta::Sha256::new();
        h.update(b"hello");
        h.digest().to_vec()
    };
    let mut out = Vec::new();
    let result = apply_paths_verified(
        None,
        &delta_path,
        &mut out,
        ChecksumAlgo::Sha256,
        None,
        Some(&after),
        &opts(),
    )
    .unwrap();
    match result {
        ApplyOutcome::Applied { checksums, .. } => {
            assert_eq!(out, b"hello");
            assert!(checksums.before.is_none(), "no source -> no before hash");
        }
        ApplyOutcome::Skipped { .. } => panic!("no source cannot skip"),
    }
}

#[test]
fn verified_sha256_matches_md5_path() {
    let dir = tempdir().unwrap();
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&src_path, SRC).unwrap();
    fs::write(&delta_path, delta_with_source()).unwrap();

    let mut h = rxdelta::Sha256::new();
    h.update(TARGET);
    let after = h.digest().to_vec();
    let mut out = Vec::new();
    let result = apply_paths_verified(
        Some(&src_path),
        &delta_path,
        &mut out,
        ChecksumAlgo::Sha256,
        None,
        Some(&after),
        &opts(),
    )
    .unwrap();
    match result {
        ApplyOutcome::Applied { checksums, .. } => {
            assert_eq!(checksums.after.unwrap().as_ref(), after);
        }
        ApplyOutcome::Skipped { .. } => panic!("expected applied"),
    }
    assert_eq!(out, TARGET);
}

#[test]
fn verified_expect_after_only_skips_source_hash_when_sizes_differ() {
    let dir = tempdir().unwrap();
    // SRC (16 B) differs in size from the target (9 B), so the source cannot be
    // an already-patched file; the skip-check source hash must not be computed.
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&src_path, SRC).unwrap();
    fs::write(&delta_path, delta_with_source()).unwrap();

    let after = md5_bytes(TARGET);
    let mut out = Vec::new();
    let result = apply_paths_verified(
        Some(&src_path),
        &delta_path,
        &mut out,
        ChecksumAlgo::Md5,
        None,
        Some(&after),
        &opts(),
    )
    .unwrap();
    match result {
        ApplyOutcome::Applied { checksums, .. } => {
            assert_eq!(out, TARGET);
            assert!(
                checksums.before.is_none(),
                "source hash must not be computed"
            );
            assert_eq!(checksums.after.unwrap().as_ref(), after);
        }
        ApplyOutcome::Skipped { .. } => panic!("different size cannot be skipped"),
    }
}

#[test]
fn verified_expect_after_only_hashes_source_when_sizes_equal() {
    let dir = tempdir().unwrap();
    // Target equals SRC in size (TARGET = "XYZ6789ab" is 9B, so use a same-size
    // source content) — the skip-check hash is computed and the check runs.
    let same_size_src: &[u8] = b"abcdefghi"; // 9 bytes, same as TARGET
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&src_path, same_size_src).unwrap();
    fs::write(&delta_path, delta_add_only_of(TARGET)).unwrap();

    let after = md5_bytes(TARGET);
    let mut out = Vec::new();
    let result = apply_paths_verified(
        Some(&src_path),
        &delta_path,
        &mut out,
        ChecksumAlgo::Md5,
        None,
        Some(&after),
        &opts(),
    )
    .unwrap();
    match result {
        ApplyOutcome::Applied { checksums, .. } => {
            assert_eq!(out, TARGET);
            assert_eq!(checksums.before.unwrap().as_ref(), md5_bytes(same_size_src));
        }
        ApplyOutcome::Skipped { .. } => panic!("different content cannot be skipped"),
    }
}

#[test]
fn delta_target_len_parses_headers_only() {
    use rxdelta::decoder::delta_target_len;
    let d = delta_with_source(); // single window, target TARGET.len()
    assert_eq!(delta_target_len(&d, 1 << 20).unwrap(), TARGET.len() as u64);

    let mut multi = vec![0xD6, 0xC3, 0xC4, 0x00, 0x00];
    multi.extend_from_slice(&WindowBuilder::new().add(b"one-").finish());
    multi.extend_from_slice(&WindowBuilder::new().add(b"two").finish());
    assert_eq!(delta_target_len(&multi, 1 << 20).unwrap(), 7);
}

#[test]
fn apply_paths_matches_apply() {
    let dir = tempdir().unwrap();
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&src_path, SRC).unwrap();
    fs::write(&delta_path, delta_with_source()).unwrap();

    let mut out_a = Vec::new();
    let mut out_b = Vec::new();
    apply(
        delta_with_source().as_slice(),
        Some(SRC),
        &mut out_a,
        &opts(),
    )
    .unwrap();
    apply_paths(Some(&src_path), &delta_path, &mut out_b, &opts()).unwrap();
    assert_eq!(out_a, out_b);
    assert_eq!(out_b, TARGET);
}

#[test]
fn expect_after_without_source_skips_not_hashing_delta_fully() {
    let dir = tempdir().unwrap();
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&delta_path, delta_add_only()).unwrap();

    // unknown expected after (not matching "hello") -> Applied path, output verified & fails
    let mut out = Vec::new();
    let err = apply_paths_verified(
        None,
        &delta_path,
        &mut out,
        ChecksumAlgo::Md5,
        None,
        Some(&md5_bytes(b"nope")),
        &opts(),
    )
    .unwrap_err();
    match &err {
        rxdelta::Error::FileChecksumMismatch { phase, .. } => assert_eq!(*phase, "output"),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn custom_checksum_plugged_via_hashing_writer() {
    // A caller-supplied Checksum implementation plugs in through HashingWriter.
    struct Xor8(u8);
    impl rxdelta::Checksum for Xor8 {
        fn update(&mut self, data: &[u8]) {
            for &b in data {
                self.0 ^= b;
            }
        }
        fn digest(self) -> Box<[u8]> {
            Box::new([self.0])
        }
    }

    let mut w = rxdelta::HashingWriter::new(Vec::new(), Xor8(0));
    w.write_all(TARGET).unwrap();
    let (inner, hash) = w.finalize();
    assert_eq!(inner, TARGET);
    assert_eq!(
        hash.digest().as_ref(),
        &[TARGET.iter().fold(0u8, |a, &b| a ^ b)]
    );
}

#[test]
fn verified_instantiate_enum_roundtrip() {
    // instantiate() returns a concrete BoxedChecksum usable with the apply flow.
    let mut h = ChecksumAlgo::Md5.instantiate();
    h.update(TARGET);
    let expected = {
        let mut m = Md5::new();
        m.update(TARGET);
        m.digest()
    };
    assert_eq!(h.digest().as_ref(), expected.as_ref());
}

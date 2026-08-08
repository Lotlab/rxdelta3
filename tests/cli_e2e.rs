use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

use rxdelta::{Checksum, Md5};

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

fn add_only_delta(target: &[u8]) -> Vec<u8> {
    let data = target;
    let mut inst = vec![1u8];
    inst.extend_from_slice(&varint(target.len() as u64));
    let addr: &[u8] = &[];

    let mut d = vec![0xD6, 0xC3, 0xC4, 0x00, 0x00];
    d.push(0); // window indicator: no source
    let enclen = 1
        + varint(target.len() as u64).len()
        + varint(data.len() as u64).len()
        + varint(inst.len() as u64).len()
        + varint(addr.len() as u64).len()
        + data.len()
        + inst.len()
        + addr.len();
    d.extend_from_slice(&varint(enclen as u64));
    d.extend_from_slice(&varint(target.len() as u64));
    d.push(0); // delta indicator: no compression
    d.extend_from_slice(&varint(data.len() as u64));
    d.extend_from_slice(&varint(inst.len() as u64));
    d.extend_from_slice(&varint(addr.len() as u64));
    d.extend_from_slice(data);
    d.extend_from_slice(&inst);
    d.extend_from_slice(addr);
    d
}

const TARGET: &[u8] = b"hello checksum world";

fn md5_hex(data: &[u8]) -> String {
    let mut h = Md5::new();
    h.update(data);
    rxdelta::encode_hex(&h.digest())
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_xdelta")
}

fn run(args: &[&str], cwd: &Path) -> (i32, String, String) {
    let out = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("failed to run binary");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn cli_plain_apply_backward_compat() {
    let dir = tempdir().unwrap();
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&delta_path, add_only_delta(TARGET)).unwrap();
    let out_path = dir.path().join("out.bin");

    let (code, _stdout, _stderr) = run(
        &[
            "apply",
            delta_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ],
        dir.path(),
    );
    assert_eq!(code, 0);
    assert_eq!(fs::read(&out_path).unwrap(), TARGET);
}

#[test]
fn cli_verified_apply_success() {
    let dir = tempdir().unwrap();
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&delta_path, add_only_delta(TARGET)).unwrap();
    let out_path = dir.path().join("out.bin");
    let after = md5_hex(TARGET);

    let (code, _stdout, stderr) = run(
        &[
            "apply",
            delta_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--algo",
            "md5",
            "--expect-after",
            &after,
        ],
        dir.path(),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(fs::read(&out_path).unwrap(), TARGET);
    assert!(
        stderr.contains(&format!("md5-after: {after}")),
        "stderr: {stderr}"
    );
}

#[test]
fn cli_verified_both_expects_all_pass() {
    let dir = tempdir().unwrap();
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    const SRC: &[u8] = b"original source data";
    const OUT: &[u8] = b"patched output data";
    fs::write(&src_path, SRC).unwrap();
    fs::write(&delta_path, add_only_delta(OUT)).unwrap();
    let out_path = dir.path().join("out.bin");
    let before = md5_hex(SRC);
    let after = md5_hex(OUT);

    let (code, _stdout, stderr) = run(
        &[
            "apply",
            "-s",
            src_path.to_str().unwrap(),
            delta_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--expect-before",
            &before,
            "--expect-after",
            &after,
        ],
        dir.path(),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(fs::read(&out_path).unwrap(), OUT);
    assert!(
        stderr.contains(&format!("md5-before: {before}")),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains(&format!("md5-after: {after}")),
        "stderr: {stderr}"
    );
}

#[test]
fn cli_skip_no_output_file() {
    let dir = tempdir().unwrap();
    // source file already holds the patched content
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&src_path, TARGET).unwrap();
    fs::write(&delta_path, add_only_delta(TARGET)).unwrap();
    let out_path = dir.path().join("out.bin");

    let (code, _stdout, stderr) = run(
        &[
            "apply",
            "-s",
            src_path.to_str().unwrap(),
            delta_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--expect-after",
            &md5_hex(TARGET),
        ],
        dir.path(),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stderr.contains("skipped: already patched"),
        "stderr: {stderr}"
    );
    assert!(!out_path.exists(), "skip must not create output file");
}

#[test]
fn cli_before_mismatch_exit_1_no_output() {
    let dir = tempdir().unwrap();
    let src_path = dir.path().join("old.bin");
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&src_path, TARGET).unwrap();
    fs::write(&delta_path, add_only_delta(TARGET)).unwrap();
    let out_path = dir.path().join("out.bin");

    let (code, _stdout, stderr) = run(
        &[
            "apply",
            "-s",
            src_path.to_str().unwrap(),
            delta_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--expect-before",
            "00000000000000000000000000000000",
        ],
        dir.path(),
    );
    assert_eq!(code, 1);
    assert!(
        stderr.contains("file checksum mismatch (source)"),
        "stderr: {stderr}"
    );
    assert!(
        !out_path.exists(),
        "source mismatch must not create output file"
    );
}

#[test]
fn cli_after_mismatch_exit_1() {
    let dir = tempdir().unwrap();
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&delta_path, add_only_delta(TARGET)).unwrap();
    let out_path = dir.path().join("out.bin");

    let (code, _stdout, stderr) = run(
        &[
            "apply",
            delta_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--expect-after",
            "00000000000000000000000000000000",
        ],
        dir.path(),
    );
    assert_eq!(code, 1);
    assert!(
        stderr.contains("file checksum mismatch (output)"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("(md5)"), "stderr: {stderr}");
}

#[test]
fn cli_bad_hex_length_exit_2() {
    let dir = tempdir().unwrap();
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&delta_path, add_only_delta(TARGET)).unwrap();
    let out_path = dir.path().join("out.bin");

    let (code, _stdout, stderr) = run(
        &[
            "apply",
            delta_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--expect-after",
            "abc",
        ],
        dir.path(),
    );
    assert_eq!(code, 2);
    assert!(stderr.contains("--expect-after"), "stderr: {stderr}");
}

#[test]
fn cli_bad_algo_exit_2() {
    let dir = tempdir().unwrap();
    let delta_path = dir.path().join("patch.vcdiff");
    fs::write(&delta_path, add_only_delta(TARGET)).unwrap();
    let out_path = dir.path().join("out.bin");

    let (code, _stdout, stderr) = run(
        &[
            "apply",
            delta_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--algo",
            "crc64",
        ],
        dir.path(),
    );
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown algorithm"), "stderr: {stderr}");
}

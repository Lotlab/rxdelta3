# rxdelta

High-speed VCDIFF (RFC 3284) patch application library and CLI, compatible with
xdelta3.

## Build

A C compiler is required because `xz2` (with the `static` feature) compiles the
vendored liblzma C sources for LZMA secondary decompression. No system liblzma
is needed.

```sh
cargo build --release
```

## Usage

```sh
# apply a patch
xdelta apply -s old.bin patch.vcdiff -o new.bin

# compression-only patch (no source)
xdelta apply patch.vcdiff -o out.bin

# skip checksum verification / show stats
xdelta apply --no-verify --stats -s old.bin patch.vcdiff -o new.bin

# file-level integrity verification (pluggable: md5 | sha256 | blake3)
xdelta apply -s old.bin patch.vcdiff -o new.bin \
    --expect-before 04f0e75c4e67aca6086e0dfa637f3bcc \
    --expect-after  1342c5bc439ae0c5716c9e1e3d781a2f

# idempotent: if the source file already has the target hash, skip applying
xdelta apply -s current.bin patch.vcdiff -o new.bin \
    --expect-after 1342c5bc439ae0c5716c9e1e3d781a2f
```

File checksum semantics:

- `--algo md5|sha256|blake3` selects the algorithm (default `md5`, implemented with
  hand-written x86_64/aarch64 assembly via `fast-md5`).
- `--expect-before <hex>` verifies the source file hash before applying; a mismatch
  aborts without creating the output file.
- `--expect-after <hex>` verifies the output hash after applying, and also enables
  the idempotent skip: if the source file already hashes to the target value, the
  patch is skipped (exit 0, no output file created).
- Performance: the source hash shares the mmap with decoding (one disk read pass);
  the output is hashed inline as it is written (never read back). When only
  `--expect-after` is given and the source size differs from the patch target size
  (i.e. the source provably cannot already be patched), the source hash is skipped
  entirely.
- Computed hashes are printed to stderr (`<algo>-before` / `<algo>-after`). The
  before hash is printed only when it was computed (i.e. `--expect-before` is given,
  or the idempotent skip check ran).

Exit codes: `0` success, `1` decode/IO/verification failure, `2` usage error.

## Compatibility

- RFC 3284 (VCDIFF): header, windows, instruction code table (default and
  application-defined), near/same address caches, ADD/COPY/RUN.
- xdelta3 extensions: per-window Adler32 checksums (`VCD_ADLER32`), LZMA
  secondary compression (xz container streams, including the continuous
  cross-section streams xdelta3 emits), appheader handling.
- Verified against real `xdelta3` 3.2.0 output over a size/content/option
  matrix (see `tests/crosscheck.sh`). This includes the 3.2.0 default "armor"
  output (BLAKE3 digests carried in the appheader, which the decoder skips).
- Not supported (clear error): DJW/FGK secondary compression (IDs 1/16),
  VCD_TARGET windows (xdelta3 itself does not produce these).

## Library

```rust
use rxdelta::{apply, ApplyOptions};

let mut out = Vec::new();
apply(delta_bytes, Some(source_bytes), &mut out, &ApplyOptions::default())?;
```

Streaming by design: peak memory is bounded by the largest window (default cap
64 MiB), independent of the total target size. Large target files (10+ GiB) are
supported; the source file is accessed through an mmap for zero-copy COPYs.

Verified apply with pluggable checksums:

```rust
use rxdelta::{apply_paths_verified, ApplyOutcome, ApplyOptions, ChecksumAlgo};

let opts = ApplyOptions::default();
let expect_after = /* 16 bytes md5 digest */;
match apply_paths_verified(
    Some("old.bin".as_ref()), "patch.vcdiff".as_ref(), &mut out,
    ChecksumAlgo::Md5, None, Some(&expect_after), &opts,
)? {
    ApplyOutcome::Applied { stats, checksums } => { /* checksums.before / .after */ }
    ApplyOutcome::Skipped { .. } => { /* already patched */ }
}
```

`Checksum` is a small trait (`update` / consuming `digest`) implemented for
`Md5`, `Sha256` and `Blake3`; library callers can supply their own implementation
on any concrete type and plug it into `HashingWriter<W, H>` / `apply`, which hash
bytes as they pass through so the output is written once and never read back.

## Tests

```sh
cargo test                      # unit + integration + fuzz-smoke
XDELTA3=/path/to/xdelta3 BIN=target/release/xdelta tests/crosscheck.sh
```

`cargo-fuzz` target lives in `fuzz/` (requires nightly + `cargo fuzz`).

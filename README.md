# rxdelta

High-speed VCDIFF (RFC 3284) patch application library and CLI, compatible with
xdelta3.

## Build

A C compiler is required because `xz2` (with the `static` feature) compiles the
vendored liblzma C sources for LZMA secondary decompression. No system liblzma
is needed.

```sh
cargo build --release                         # xdelta CLI only
cargo build --release -p xdelta3-wrap --target i686-pc-windows-msvc        # wrapper DLL
cargo build --release -p xdelta3-installer --target x86_64-pc-windows-msvc # installer

# or use the full build script (builds all three, outputs to target/delivery/):
scripts/build-installer.sh
```

The delivery script produces:

- `target/delivery/XDelta3WrapFactory.dll` -- renamed wrapper DLL (drop-in replacement)
- `target/delivery/xdelta.exe` -- 64-bit apply helper (used by the wrapper for large files)
- `target/delivery/xdelta3-installer.exe` -- self-contained installer with embedded artifacts

## CLI Usage

```
xdelta [OPTIONS] <SUBCOMMAND>

Subcommands:
  apply    Apply a delta patch to a source file
```

### `xdelta apply`

```
xdelta apply [OPTIONS] <DELTA>

Arguments:
  <DELTA>                 Delta (patch) file

Options:
  -s, --source <PATH>     Source file (omit for compression-only patches)
  -o, --output <PATH>     Output file (defaults to stdout)
  --no-verify             Skip window checksum verification
  --stats                 Print apply statistics to stderr
  --algo <ALGO>           Checksum algorithm: md5 | sha256 | blake3 [default: md5]
  --expect-before <HEX>   Expected source checksum (pre-verify + idempotent skip)
  --expect-after <HEX>    Expected output checksum (skip + post-verify)
  --no-skip-check         Skip idempotent source-hash skip check
  --in-place              Rewrite source file in place (only changed windows)
  -h, --help              Print help
```

### Examples

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

# in-place patch: rewrite only changed windows (no separate output file)
xdelta apply --in-place -s target.bin patch.vcdiff \
    --expect-after 1342c5bc439ae0c5716c9e1e3d781a2f
```

### File Checksum Semantics

- `--algo md5|sha256|blake3` selects the algorithm (default `md5`, implemented with
  hand-written x86_64/aarch64 assembly via `fast-md5`).
- `--expect-before <hex>` verifies the source file hash before applying; a mismatch
  aborts without creating the output file.
- `--expect-after <hex>` verifies the output hash after applying, and also enables
  the idempotent skip: if the source file already hashes to the target value, the
  patch is skipped (exit 0, no output file created).
- `--no-skip-check` disables the idempotent skip check but still verifies the output
  hash. Use when the source is known to be the old version and you want to force
  re-apply.
- `--in-place` rewrites the source file directly, updating only the window slots
  that changed. The source and output must be the same file. This is used by the
  xdelta3-wrap DLL for in-game patching.
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

## Workspace Crates

| Crate | Type | Output | Target |
|-------|------|--------|--------|
| `rxdelta` | lib + bin | `xdelta.exe` | host |
| `xdelta3-wrap` | cdylib | `xdelta3_wrap.dll` | `i686-pc-windows-msvc` |
| `xdelta3-installer` | bin | `xdelta3-installer.exe` | `x86_64-pc-windows-msvc` |

### xdelta3-wrap

A drop-in replacement for the `XDelta3WrapFactory.dll` used by the FFXIV
LauncherV3 (CN). It reimplements the merge logic in Rust on top of the `rxdelta`
library, providing the same exports while using the high-performance decoder
internally.

The DLL is built as a 32-bit cdylib (`i686-pc-windows-msvc`) and exports the
same 9 `__stdcall` symbols as the original DLL. The launcher calls these to
apply delta patches during game updates.

**Exports (implemented):**

- `MergeFile(src, delta, dst)` -- single-file VCDIFF merge
- `MergeDir(src_dir, patch_dir, dst_dir, file_cb)` -- directory merge
- `MergeDirCustomDiff(src_dir, patch_dir, dst_dir, file_cb, merge_cb)` -- directory merge with custom callback
- `MergeDirCustomDiffV2(src_dir, patch_dir, dst_dir, file_cb, cmd_cb, merge_cb)` -- directory merge with progress + custom callback

**Key features:**

- **In-place apply**: when `src == dst`, rewrites only changed window slots of the source file.
- **Large file delegation**: files > 256 MB are delegated to the 64-bit `xdelta.exe` subprocess (32-bit process cannot mmap them).
- **Idempotent skip**: skips files that already match the target hash (MD5).
- **Skip-copy optimization**: when source and destination directories differ, copies the source tree first, skipping files that will be produced by delta patching.
- **Chinese progress messages**: matches the original DLL's callback message templates exactly.
- Logs to `xdelta3_wrap.log` next to the host process.

**Build:**

```sh
cargo build --release -p xdelta3-wrap --target i686-pc-windows-msvc
```

### xdelta3-installer

A standalone tool that installs or rolls back the xdelta3-wrap DLL into the
FFXIV LauncherV3 directory. Supports two modes:

**CLI mode** (from a terminal):

```
xdelta3-installer [COMMAND] [OPTIONS]

Commands:
  install    Replace XDelta3WrapFactory.dll with xdelta3_wrap.dll and
             install xdelta.exe into Launcher3Modules/ (default)
  rollback   Restore the original DLL and remove installed files

Options:
  --target <DIR>     Launcher directory (default: this exe's directory)
  --dll <PATH>       xdelta3_wrap.dll source (when not embedded)
  --xdelta <PATH>    xdelta.exe source (when not embedded)
  -h, --help         Show this help
```

**GUI mode** (double-click):

- No arguments: if no manifest exists at the target, installs; otherwise rolls back.
- Shows a native `MessageBoxW` with the result.

**Install flow:**

1. Validates the target by checking for `Launcher3Configs/LauncherConfig.xml`.
2. Backs up the original `Launcher3Modules/XDelta3WrapFactory.dll` to `.rxdelta3/backup/`.
3. Writes the new DLL and `xdelta.exe` into `Launcher3Modules/`.
4. Saves a manifest to `.rxdelta3/manifest.json`.

**Rollback flow:** restores each backup, removes installed files, deletes the manifest.

**Build:**

```sh
# see scripts/build-installer.sh for the full pipeline
cargo build --release -p xdelta3-installer --target x86_64-pc-windows-msvc
```

The installer binary embeds both the 32-bit wrapper DLL and the 64-bit `xdelta.exe`
at build time (via `build.rs`), so the standalone `.exe` can be distributed as a
single self-contained file.

## Tests

```sh
cargo test                      # unit + integration + fuzz-smoke
XDELTA3=/path/to/xdelta3 BIN=target/release/xdelta tests/crosscheck.sh
```

`cargo-fuzz` target lives in `fuzz/` (requires nightly + `cargo fuzz`).

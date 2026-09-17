# In-place apply benchmarks

Tracks `xdelta apply --in-place` performance across versions so that a new
change can be judged against recorded numbers instead of re-running old
binaries. All timings below are **apply-only medians** (setup excluded).

## Versions

| Label | Description |
|---|---|
| ref | Baseline: stock xdelta3 CLI full decode (`decode -s OLD DELTA OUT`, whole target rewritten). This is what "baseline" means in this document. |
| v0.1 | Pre-change implementation: two-phase in-place apply, checksum verified *after* writing (commit `2b3518f`) |
| v0.2 | Journal-backed rollback + `expect_after` pre-write verify + fused inline hashing (commit `c3335fc`) |

## Method (`scripts/bench_inplace.sh`)

- Release builds (`cargo build --release --bin xdelta`).
- 1 warmup + N timed runs per case; median reported.
- Every run starts from a fresh copy of the old file (**copy time excluded**);
  `sync` between runs keeps writeback debt out of the measured window.
- `cmp` correctness gate on every run: result must equal the expected new
  file byte-for-byte, otherwise the run is rejected.
- Only the `apply` invocation itself is timed — file copies, hashing of
  reference files, and verification reads are not included.

Modes:

| Mode | Flags | Meaning |
|---|---|---|
| `plain` | (none) | No checksums |
| `afteronly` | `--expect-after <new> --no-skip-check` | Patch-runner profile: source known old, output verified |
| `both` | `--expect-before <old> --expect-after <new>` | Full verification incl. idempotent skip |

## Fixtures (no machine paths; pass yours via CLI args)

| Label | Description | Layout (from `scan_layout`) |
|---|---|---|
| S1 | Synthetic 256 MiB, 25% of 64 KiB blocks replaced with random bytes (seed-fixed, see `scripts/mkfixture.py`), delta produced by the reference encoder | 32 windows, all rewritten (full rewrite) |
| S2 | Synthetic 256 MiB, 1% of blocks replaced, reference delta | 32 windows, 22 rewritten (~184 MB), 10 skipped |
| R1 | Real game data, version 2026.08.05.0000.0000 → 2026.09.01.0000.0000, file `game/sqpack/ffxiv/0a0000.win32.dat0` (~150 MB, grows ~2 MB) | ~9k windows, ~1.3k rewritten (~22 MB), rest skipped |
| R2 | Real game data, version 2026.07.16.0001.0000 → 2026.08.05.0000.0000, file `game/sqpack/ffxiv/040000.win32.dat0` (~12.4 GB, grows ~0.2 MB), delta from patch package 0.0.0.25 → 0.0.0.26 | ~810k windows, 50 rewritten (<1 MB), rest skipped |

S1/S2 ran on a memory filesystem; R1/R2 on NVMe/ext4. Absolute numbers
depend on hardware — compare **relative** deltas, not absolutes.

## Results

One row per version — append a row for each new version. All values are
medians; the unit (ms or s) is noted on each table heading. `speedup vs
baseline` = baseline ÷ version on `plain` (>1× means faster than xdelta3,
unitless). `—` means not applicable (`ref` has no verify modes) or not
measured (R2 `both`, skipped: same cost as `afteronly` plus one
source-hash pass).

### S1 — synthetic 256 MiB, full rewrite (ms)

| Version | plain | afteronly | both | speedup vs baseline |
|---|---|---|---|---|
| ref (xdelta3) | 386.5 | — | — | 1.00× |
| v0.1 | 315 | 629 | 615 | 1.23× |
| v0.2 | 478 | 756 | 1045 | 0.81× |

### S2 — synthetic 256 MiB, 1% changed (ms)

| Version | plain | afteronly | both | speedup vs baseline |
|---|---|---|---|---|
| ref (xdelta3) | 348 | — | — | 1.00× |
| v0.1 | 210 | 513 | 519 | 1.66× |
| v0.2 | 322 | 607 | 884 | 1.08× |

### R1 — real data ~150 MB, ~22 MB rewritten (ms)

| Version | plain | afteronly | both | speedup vs baseline |
|---|---|---|---|---|
| ref (xdelta3) | 202 | — | — | 1.00× |
| v0.1 | 33 | 203 | 239 | 6.12× |
| v0.2 | 97 | 260 | 414 | 2.08× |

### R2 — real data ~12.4 GB, <1 MB rewritten (s)

| Version | plain | afteronly | both | speedup vs baseline |
|---|---|---|---|---|
| ref (xdelta3) | 50.7 | — | — | 1.00× |
| v0.1 | 0.09 | 22.9 | — | 557× |
| v0.2 | 0.15 | 20.1 | — | 327× |

## Adding a new version row

Build the new binary, then run single-version mode (omit `--bin-b`):

```bash
cargo build --release --bin xdelta
scripts/bench_inplace.sh \
  --old <old-file> --new <new-file> --delta <delta> \
  --bin-a target/release/xdelta --label-a v0.3 \
  --modes plain,afteronly,both --reps 5 --workdir <scratch-dir>
```

Append one row per fixture table with the medians. For synthetic fixtures,
regenerate with `scripts/mkfixture.py <size-mb> <fraction> <seed> <prefix>`
and the reference encoder. To (re-)record the xdelta3 baseline for a
fixture, run with `--modes ref --ref-bin <path-to-xdelta3>` (needs no
`--bin-a`).

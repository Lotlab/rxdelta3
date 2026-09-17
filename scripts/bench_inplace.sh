#!/usr/bin/env bash
# Benchmark driver for `xdelta apply --in-place`.
#
# Compares two binaries (e.g. pre-change vs post-change) or measures a single
# binary (for recording a new version row without re-running old binaries).
# Paths are passed in — nothing machine-specific is hardcoded.
#
# Usage:
#   bench_inplace.sh --old OLD --new NEW --delta DELTA \
#       --bin-a PATH [--label-a NAME] \
#       [--bin-b PATH [--label-b NAME]] \
#       [--modes plain,afteronly,both] [--reps N] [--workdir DIR]
#
# Modes:
#   plain      no checksum flags
#   afteronly  --expect-after <new-md5> --no-skip-check (patch-runner profile)
#   both       --expect-before <old-md5> --expect-after <new-md5>
#   ref        reference decoder (`--ref-bin`, traditionally the stock xdelta3
#              CLI: full rewrite via `decode -s OLD DELTA OUT`). This is the
#              true baseline everything else is compared against. May be the
#              only mode (`--modes ref` needs no --bin-a).
#
# Method (see docs/benchmark.md): release binaries, 1 warmup + N timed runs,
# fresh copy of OLD per run (copy time excluded), `sync` between runs to keep
# writeback debt out of the measured window, `cmp` correctness gate on every
# run, median reported. Only the apply itself is timed.
set -euo pipefail

OLD=""; NEW=""; DELTA=""
BIN_A=""; LABEL_A="a"; BIN_B=""; LABEL_B="b"; REF_BIN="${XDELTA3_BIN:-}"
MODES="plain,afteronly,both"; REPS=4; WORKDIR=""

while [ $# -gt 0 ]; do
  case "$1" in
    --old) OLD="$2"; shift 2;;
    --new) NEW="$2"; shift 2;;
    --delta) DELTA="$2"; shift 2;;
    --bin-a) BIN_A="$2"; shift 2;;
    --label-a) LABEL_A="$2"; shift 2;;
    --bin-b) BIN_B="$2"; shift 2;;
    --label-b) LABEL_B="$2"; shift 2;;
    --ref-bin) REF_BIN="$2"; shift 2;;
    --modes) MODES="$2"; shift 2;;
    --reps) REPS="$2"; shift 2;;
    --workdir) WORKDIR="$2"; shift 2;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

[ -n "$OLD" ] && [ -n "$NEW" ] && [ -n "$DELTA" ] || {
  echo "missing required args (see header)" >&2; exit 2; }
case ",$MODES," in
  *,ref,*) [ -n "$REF_BIN" ] || { echo "ref mode needs --ref-bin" >&2; exit 2; };;
esac
case ",$MODES," in
  *,plain,*|*,afteronly,*|*,both,*)
    [ -n "$BIN_A" ] || { echo "missing --bin-a (see header)" >&2; exit 2;};;
esac
[ -z "$WORKDIR" ] && WORKDIR="$(mktemp -d)"
mkdir -p "$WORKDIR"

OLD_MD5="$(md5sum "$OLD" | cut -d' ' -f1)"
NEW_MD5="$(md5sum "$NEW" | cut -d' ' -f1)"

median_of() { # values... -> median (numeric sort; averages middle two)
  printf '%s\n' "$@" | sort -n | awk -v n="$#" '{a[NR]=$1} END {print (n%2 ? a[(n+1)/2] : (a[n/2]+a[n/2+1])/2)}'
}

# Reference decoder baseline: full rewrite via `$REF_BIN -d -s OLD DELTA OUT`
# (stock xdelta3 CLI). Same methodology: warmup, fresh output per run,
# sync between runs, cmp gate, median.
run_ref() {
  rm -f "$WORKDIR/ref.out"
  sync
  "$REF_BIN" -d -s "$OLD" "$DELTA" "$WORKDIR/ref.out" >/dev/null 2>&1
  cmp -s "$WORKDIR/ref.out" "$NEW" || { echo "ref: WRONG-OUTPUT" >&2; return 1; }
  local times=()
  for ((i=1; i<=REPS; i++)); do
    rm -f "$WORKDIR/ref.out"
    sync
    local t0 t1
    t0=$(date +%s%N)
    "$REF_BIN" -d -s "$OLD" "$DELTA" "$WORKDIR/ref.out" >/dev/null 2>&1
    t1=$(date +%s%N)
    cmp -s "$WORKDIR/ref.out" "$NEW" || { echo "ref: WRONG-OUTPUT" >&2; return 1; }
    times+=($(( (t1 - t0) / 1000000 )))
  done
  echo "ref: ${times[*]} ms (median $(median_of "${times[@]}") ms)"
  rm -f "$WORKDIR/ref.out"
}

one_run() { # label bin [extra args...]
  local label=$1 bin=$2; shift 2
  cp "$OLD" "$WORKDIR/work.bin"
  sync
  local t0 t1
  t0=$(date +%s%N)
  "$bin" apply --in-place -s "$WORKDIR/work.bin" "$DELTA" "$@" >/dev/null 2>&1
  t1=$(date +%s%N)
  if ! cmp -s "$WORKDIR/work.bin" "$NEW"; then
    echo "$label: WRONG-OUTPUT (result != expected new file)" >&2
    return 1
  fi
  echo $(( (t1 - t0) / 1000000 ))
}

run_mode() { # mode-name... (flag args passed as "$@")
  local mode=$1; shift
  local labels=("$LABEL_A") bins=("$BIN_A")
  [ -n "$BIN_B" ] && { labels+=("$LABEL_B"); bins+=("$BIN_B"); }
  for idx in "${!bins[@]}"; do
    local label="${labels[$idx]}/$mode" bin="${bins[$idx]}"
    one_run "$label/warmup" "$bin" "$@" >/dev/null || return 1  # warmup, untimed
    local times=()
    for ((i=1; i<=REPS; i++)); do
      times+=("$(one_run "$label" "$bin" "$@")") || return 1
    done
    echo "$label: ${times[*]} ms (median $(median_of "${times[@]}") ms)"
  done
}

IFS=',' read -ra MODELlIST <<< "$MODES"
for m in "${MODELlIST[@]}"; do
  case "$m" in
    plain) run_mode plain;;
    afteronly) run_mode afteronly --no-skip-check --expect-after "$NEW_MD5";;
    both) run_mode both --expect-before "$OLD_MD5" --expect-after "$NEW_MD5";;
    ref) run_ref;;
    *) echo "unknown mode: $m" >&2; exit 2;;
  esac
done
rm -f "$WORKDIR/work.bin"

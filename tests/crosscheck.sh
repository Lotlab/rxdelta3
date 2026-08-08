#!/usr/bin/env bash
# Cross-validation against a real xdelta3 binary.
#   Usage: XDELTA3=/path/to/xdelta3 BIN=target/release/xdelta tests/crosscheck.sh
# Generates source/target pairs, creates patches with xdelta3 under a matrix of
# options, applies them with our tool, and byte-compares the output.
set -u

XD3="${XDELTA3:-xdelta3}"
BIN="${BIN:-target/release/xdelta}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if ! command -v "$XD3" >/dev/null 2>&1; then
  echo "SKIP: xdelta3 not found (set XDELTA3=/path/to/xdelta3)"
  exit 0
fi
if [ ! -x "$BIN" ]; then
  echo "SKIP: build the tool first (cargo build --release)"
  exit 0
fi

pass=0
fail=0

# gen <file> <size> <kind>
gen() {
  python3 - "$1" "$2" "$3" <<'EOF'
import random, sys
out, size, kind = sys.argv[1], int(sys.argv[2]), sys.argv[3]
r = random.Random(12345)
if kind == "random":
    data = bytes(r.randrange(256) for _ in range(size))
elif kind == "zeros":
    data = bytes(size)
elif kind == "periodic":
    data = (b"abracadabra\x00\xff" * (size // 14 + 1))[:size]
elif kind == "text":
    data = (b"The quick brown fox jumps over the lazy dog\n" * (size // 45 + 1))[:size]
else:
    data = bytes(r.randrange(256) for _ in range(size))
open(out, "wb").write(data)
EOF
}

# mktarget <src> <tgt>  : derive a target from the source
mktarget() {
  python3 - "$1" "$2" <<'EOF'
import random, sys
src = open(sys.argv[1], "rb").read()
r = random.Random(99)
t = bytearray(src)
n = len(t)
if n > 0:
    for _ in range(max(1, n // 200)):
        pos = r.randrange(n)
        t[pos:pos + r.randrange(1, 30)] = bytes(r.randrange(256) for _ in range(r.randrange(1, 40)))
    if n > 100:
        pos = r.randrange(n)
        ins = bytes(r.randrange(256) for _ in range(50))
        t[pos:pos] = ins
# duplicate a large region (overlapping copy opportunities)
if n > 500:
    srcr = bytes(t[: n // 3])
    t.extend(srcr)
open(sys.argv[2], "wb").write(bytes(t))
EOF
}

# run <name> <src-or-empty> <tgt> <xd3-extra-opts> <expect-fail>
run() {
  local name="$1" src="$2" tgt="$3" opts="$4" expect_fail="$5"
  local patch="$WORK/p.vcdiff"
  if [ -n "$src" ]; then
    "$XD3" -e -f -q $opts -s "$src" "$tgt" "$patch" >/dev/null 2>&1 || { echo "SKIP $name (encode failed)"; return; }
    "$BIN" apply -s "$src" "$patch" -o "$WORK/out.bin" >/dev/null 2>&1
  else
    "$XD3" -e -f -q $opts "$tgt" "$patch" >/dev/null 2>&1 || { echo "SKIP $name (encode failed)"; return; }
    "$BIN" apply "$patch" -o "$WORK/out.bin" >/dev/null 2>&1
  fi
  local rc=$?
  if [ "$expect_fail" = "yes" ]; then
    if [ "$rc" -eq 0 ]; then
      echo "FAIL $name (expected apply to fail, but it succeeded)"
      fail=$((fail + 1))
    else
      echo "PASS $name (correctly rejected)"
      pass=$((pass + 1))
    fi
    return
  fi
  if [ "$rc" -ne 0 ]; then
    echo "FAIL $name (apply failed rc=$rc)"
    fail=$((fail + 1))
  elif cmp -s "$tgt" "$WORK/out.bin"; then
    echo "PASS $name"
    pass=$((pass + 1))
  else
    echo "FAIL $name (output mismatch)"
    fail=$((fail + 1))
  fi
}

S="$WORK/source.bin"
T="$WORK/target.bin"

for size in 0 1 100 4096 65536 1000000; do
  for kind in random zeros periodic text; do
    [ "$size" = "0" ] && [ "$kind" != "random" ] && continue
    gen "$S" "$size" "$kind"
    if [ "$size" = "0" ]; then
      cp /dev/null "$T"
    else
      mktarget "$S" "$T"
    fi
    run "default-armor s=$size $kind" "$S" "$T" "" no
    run "no-armor s=$size $kind" "$S" "$T" "-a" no
    run "no-checksum s=$size $kind" "$S" "$T" "-a -n" no
    run "djw s=$size $kind" "$S" "$T" "-a -S djw" yes
    run "fgk s=$size $kind" "$S" "$T" "-a -S fgk" yes
  done
done

# tiny windows / many windows
gen "$S" 300000 random
mktarget "$S" "$T"
run "tiny-windows" "$S" "$T" "-a -W 16384 -B 524288" no
run "many-windows-default" "$S" "$T" "" no

# compression-only
gen "$S" 0 random
gen "$T" 200000 random
run "compression-only" "" "$T" "-a" no

# empty target
gen "$S" 50000 random
cp /dev/null "$T"
run "empty-target" "$S" "$T" "-a" no

echo
echo "PASS=$pass FAIL=$fail"
[ "$fail" -eq 0 ]

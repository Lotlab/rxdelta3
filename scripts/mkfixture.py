#!/usr/bin/env python3
"""Generate a synthetic old/new file pair for in-place benchmarks.

The new file equals the old file with a fraction of 64 KiB blocks replaced
by fresh random bytes. Build a delta between them with e.g.:
    xdelta3 -e -s <prefix>.old <prefix>.new <prefix>.delta

Usage: mkfixture.py <size-mb> <change-fraction> <seed> <prefix>
"""
import os
import random
import sys


def main() -> None:
    size_mb, frac, seed, prefix = int(sys.argv[1]), float(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
    random.seed(seed)
    blk = 65536
    n = size_mb * 1024 * 1024
    old = bytearray(os.urandom(n))
    new = bytearray(old)
    nblk = n // blk
    changed = random.sample(range(nblk), int(nblk * frac))
    for i in changed:
        new[i * blk:(i + 1) * blk] = os.urandom(blk)
    with open(f"{prefix}.old", "wb") as f:
        f.write(old)
    with open(f"{prefix}.new", "wb") as f:
        f.write(new)
    print(f"{prefix}: {n} bytes, {len(changed)}/{nblk} blocks changed")


if __name__ == "__main__":
    main()

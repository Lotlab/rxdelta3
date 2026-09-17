#!/usr/bin/env bash
# Builds the deployable xdelta3-installer.exe with both artifacts embedded.
#
# Prereqs (run once):
#   rustup target add i686-pc-windows-msvc x86_64-pc-windows-msvc
# Requires a Windows MSVC toolchain (native Windows, or Linux with cargo-xwin).
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET_I686=i686-pc-windows-msvc
TARGET_X64=x86_64-pc-windows-msvc

# 1. 32-bit wrapper DLL (xdelta3_wrap.dll)
cargo build --release -p xdelta3-wrap --target "$TARGET_I686"

# 2. 64-bit apply helper (xdelta.exe)
cargo build --release -p rxdelta --bin xdelta --target "$TARGET_X64"

# 3. Installer with both artifacts embedded (explicit env for reproducibility)
export XDELTA3_WRAP_DLL="target/$TARGET_I686/release/xdelta3_wrap.dll"
export XDELTA_EXE="target/$TARGET_X64/release/xdelta.exe"

cargo build --release -p xdelta3-installer --target "$TARGET_X64"

echo "built: target/$TARGET_X64/release/xdelta3-installer.exe"

DELIVERY=target/delivery

mkdir -p $DELIVERY
cp $XDELTA3_WRAP_DLL $DELIVERY/XDelta3WrapFactory.dll
cp $XDELTA_EXE $DELIVERY/xdelta.exe
cp target/$TARGET_X64/release/xdelta3-installer.exe $DELIVERY/xdelta3-installer.exe


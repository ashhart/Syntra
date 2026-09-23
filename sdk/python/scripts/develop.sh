#!/bin/sh
# Build the native extension with cargo and drop it into the package so
# `python/` can be imported directly (tests, local use). Wheels for
# distribution come from `maturin build --release` instead.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
cargo build --release --manifest-path "$here/Cargo.toml"
case "$(uname -s)" in
  Darwin) lib=lib_native.dylib ;;
  Linux) lib=lib_native.so ;;
  *) echo "develop.sh: unsupported platform $(uname -s)" >&2; exit 1 ;;
esac
cp "$here/target/release/$lib" "$here/python/syntra/_native.abi3.so"
echo "built $here/python/syntra/_native.abi3.so"

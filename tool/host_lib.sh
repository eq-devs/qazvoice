#!/usr/bin/env bash
# Builds the native library for this computer (macOS / Linux) so the Dart tests
# in test/native_test.dart can run the real engine:
#   tool/host_lib.sh && QAZVOICE_LIB=$(tool/host_lib.sh --path) QAZVOICE_MODELS=… flutter test test/native_test.dart
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
EXT=so; [ "$(uname)" = Darwin ] && EXT=dylib
if [ "${1:-}" = --path ]; then echo "$ROOT/rust/target/host/release/libqazvoice.$EXT"; exit 0; fi
cd "$ROOT/rust"
FLAGS=""
if [ "$(uname)" = Darwin ]; then
  # ONNX Runtime's CoreML code needs compiler-rt's __isPlatformVersionAtLeast.
  FLAGS="-C link-arg=$(clang -print-resource-dir)/lib/darwin/libclang_rt.osx.a"
fi
RUSTFLAGS="$FLAGS" CARGO_TARGET_DIR=target/host cargo build --release -p qazvoice-ffi

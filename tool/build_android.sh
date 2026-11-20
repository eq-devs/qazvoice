#!/usr/bin/env bash
# Builds the native library (libqazvoice.so) for every Android ABI the plugin ships
# and puts it into android/src/main/jniLibs. Plugin users do NOT need this: the
# prebuilt libraries are part of the package. Run it after changing rust/.
#
# Needs: Rust (rustup targets aarch64-linux-android armv7-linux-androideabi
# x86_64-linux-android), cargo-ndk, Android NDK r28+ (16 KB page alignment).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
: "${ANDROID_HOME:=$HOME/Library/Android/sdk}"
if [ -z "${ANDROID_NDK_HOME:-}" ]; then
  ANDROID_NDK_HOME=$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)
fi
export ANDROID_NDK_HOME
OUT="$ROOT/android/src/main/jniLibs"
rm -rf "$OUT"
cd "$ROOT/rust"
cargo ndk --platform 24 -t arm64-v8a -t armeabi-v7a -t x86_64 -o "$OUT" \
  build --release -p qazvoice-ffi --no-default-features --features onnx-dynamic
# Check 16 KB alignment of the 64-bit libraries (required by Android 15+ devices).
READELF=$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-readelf | head -1)
for abi in arm64-v8a x86_64; do
  if "$READELF" -lW "$OUT/$abi/libqazvoice.so" | awk '/LOAD/ {print $NF}' | grep -qv 0x4000; then
    echo "error: $abi/libqazvoice.so is not 16 KB aligned"; exit 1
  fi
done
ls -la "$OUT"/*/libqazvoice.so

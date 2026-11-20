#!/usr/bin/env bash
# Builds ios/qazvoice/Frameworks/QazVoiceNative.xcframework: a dynamic framework
# (libqazvoice + ONNX Runtime linked in statically from the official
# onnxruntime-c pod archive) for iOS devices and the simulator (arm64).
# A dynamic framework keeps its exported symbols, so dart:ffi finds them in any
# app build configuration. Plugin users do NOT need this script.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
ORT_VER=1.22.0
MIN_IOS=15.1
O=$ROOT/rust/target/ios-ort
if [ ! -s "$O/dev/libonnxruntime.a" ]; then
  mkdir -p "$O/sim" "$O/dev"
  curl -fsSL -o "$O/ort.zip" "https://download.onnxruntime.ai/pod-archive-onnxruntime-c-$ORT_VER.zip"
  unzip -q -o "$O/ort.zip" -d "$O/x"
  X=$O/x/onnxruntime.xcframework
  lipo -thin arm64 "$X/ios-arm64_x86_64-simulator/onnxruntime.framework/onnxruntime" -output "$O/sim/libonnxruntime.a"
  lipo -thin arm64 "$X/ios-arm64/onnxruntime.framework/onnxruntime" -output "$O/dev/libonnxruntime.a"
fi
RT=$(clang -print-resource-dir)/lib/darwin
cd "$ROOT/rust"
export CARGO_TARGET_DIR=$ROOT/rust/target/ios
export IPHONEOS_DEPLOYMENT_TARGET=$MIN_IOS
STAGE=$ROOT/rust/target/ios-frameworks
rm -rf "$STAGE"; mkdir -p "$STAGE"
for spec in aarch64-apple-ios:dev:ios:iphoneos aarch64-apple-ios-sim:sim:iossim:iphonesimulator; do
  IFS=: read -r target ort rt platform <<<"$spec"
  # compiler-rt provides __isPlatformVersionAtLeast for ONNX Runtime's CoreML code.
  ORT_LIB_LOCATION=$O/$ort ORT_PREFER_DYNAMIC_LINK=0 cargo rustc --release -p qazvoice-ffi --lib \
    --crate-type cdylib --target "$target" --no-default-features --features onnx-static -- \
    -C link-arg="$RT/libclang_rt.$rt.a" -C link-arg=-Wl,-install_name,@rpath/QazVoiceNative.framework/QazVoiceNative \
    -C link-arg=-framework -C link-arg=Foundation -C link-arg=-framework -C link-arg=CoreML -C link-arg=-lc++
  FW=$STAGE/$platform/QazVoiceNative.framework
  mkdir -p "$FW"
  cp "$CARGO_TARGET_DIR/$target/release/libqazvoice.dylib" "$FW/QazVoiceNative"
  strip -x "$FW/QazVoiceNative" 2>/dev/null || true
  cat > "$FW/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>QazVoiceNative</string>
  <key>CFBundleIdentifier</key><string>kz.qazvoice.native</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>QazVoiceNative</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>MinimumOSVersion</key><string>$MIN_IOS</string>
  <key>CFBundleSupportedPlatforms</key><array><string>$( [ $platform = iphoneos ] && echo iPhoneOS || echo iPhoneSimulator )</string></array>
</dict></plist>
PLIST
done
OUT=$ROOT/ios/qazvoice/Frameworks/QazVoiceNative.xcframework
rm -rf "$OUT" "$ROOT/ios/Frameworks"
mkdir -p "$(dirname "$OUT")"
xcodebuild -create-xcframework \
  -framework "$STAGE/iphoneos/QazVoiceNative.framework" \
  -framework "$STAGE/iphonesimulator/QazVoiceNative.framework" \
  -output "$OUT" >/dev/null
du -sh "$OUT"/*/QazVoiceNative.framework/QazVoiceNative
nm -gU "$OUT/ios-arm64/QazVoiceNative.framework/QazVoiceNative" | grep -c " _qv_"

#!/usr/bin/env bash
# macOS only (iconutil, plutil). Proof is dot-4uzgol.15. Usage: make-app.sh <release-binary> <out-dir>
set -euo pipefail
[ "$(uname)" = Darwin ] || { echo "make-app: macOS only" >&2; exit 1; }
. "$(dirname "$0")/sign.sh"
bin=$1
out=$2
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app="$out/Oats Meetings.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/oats-meetings-rs"
# sherpa-onnx (Parakeet / Moonshine, CoreML) is linked as dylibs on macOS (rpath @loader_path): ship them beside the binary.
for lib in libsherpa-onnx-c-api.dylib libonnxruntime.dylib; do
  [ -f "$(dirname "$bin")/$lib" ] || { echo "make-app: $lib not found beside $bin (build with the parakeet feature)" >&2; exit 1; }
  cp "$(dirname "$bin")/$lib" "$app/Contents/MacOS/"
done
otool -l "$app/Contents/MacOS/oats-meetings-rs" | grep -q "@loader_path" || { echo "make-app: binary lacks the @loader_path rpath" >&2; exit 1; }
sed "s/@VERSION@/$ver/" packaging/macos/Info.plist.in > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
# TCC needs these strings or macOS kills the app on first capture (dot-4uzgol.6).
for k in NSMicrophoneUsageDescription NSScreenCaptureUsageDescription NSAudioCaptureUsageDescription; do
  plutil -extract "$k" raw "$app/Contents/Info.plist" >/dev/null || { echo "make-app: Info.plist lacks $k" >&2; exit 1; }
done
# Icon A (assets/icon/AppIcon.png, 1024px): iconset sizes 16..512 at @1x/@2x via sips, then iconutil.
iconset="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$iconset"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" assets/icon/AppIcon.png --out "$iconset/icon_${s}x${s}.png" >/dev/null
  sips -z "$((s*2))" "$((s*2))" assets/icon/AppIcon.png --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
plutil -extract CFBundleIconFile raw "$app/Contents/Info.plist" | grep -qx AppIcon || { echo "make-app: Info.plist CFBundleIconFile is not AppIcon" >&2; exit 1; }
# Round trip: the icns must reopen as an iconset.
iconutil -c iconset "$app/Contents/Resources/AppIcon.icns" -o "$out/roundtrip.iconset"
ls "$out/roundtrip.iconset"
# Ad-hoc sign with an identifier-only DR so TCC grants survive rebuilds (dot-4uzgol.74, sign.sh).
sign_app "$app"

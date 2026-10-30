#!/usr/bin/env bash
# macOS only (iconutil, plutil). Proof is dot-4uzgol.15. Usage: make-app.sh <release-binary> <out-dir>
set -euo pipefail
bin=$1
out=$2
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app="$out/Oats Meetings.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/oats-meetings-rs"
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
# Sign the bundle so TCC grants stick to one identity (CODESIGN_IDENTITY: a Developer ID, or "-" ad hoc).
codesign --force --deep --sign "${CODESIGN_IDENTITY:--}" "$app"
codesign --verify --deep --strict "$app"

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
[ -d assets/icon/derived/icon.iconset ] || bash ci/icons.sh
iconutil -c icns assets/icon/derived/icon.iconset -o "$app/Contents/Resources/AppIcon.icns"
# Round trip: the icns must reopen as an iconset.
iconutil -c iconset "$app/Contents/Resources/AppIcon.icns" -o "$out/roundtrip.iconset"
ls "$out/roundtrip.iconset"
# Sign the bundle so TCC grants stick to one identity (CODESIGN_IDENTITY: a Developer ID, or "-" ad hoc).
codesign --force --deep --sign "${CODESIGN_IDENTITY:--}" "$app"
codesign --verify --deep --strict "$app"

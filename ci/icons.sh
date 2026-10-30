#!/usr/bin/env bash
# Derive every icon format from the committed 1024px icon A source (blueprint icon, dot-4uzgol.12). Pure function of assets/icon/AppIcon.png.
# Run in a pod: bin/pod-run <repo> <ref> bash ci/icons.sh   (outputs under assets/icon/derived/)
# The .icns itself (iconutil -c icns) is macOS-only and is proven by dot-4uzgol.15; this emits the iconset inputs.
set -euo pipefail
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
src=assets/icon/AppIcon.png
want=3ca009ee245f9cf95ac7157a7e860d3156af6e9eb0c43a5012ccc53df65920d5
[ "$(sha256sum "$src" | cut -d' ' -f1)" = "$want" ] || { echo "icons.sh: $src sha256 differs from the pinned icon A" >&2; exit 1; }
im=$(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#imagemagick" | head -1)
export PATH="$im/bin:$PATH"
out=${1:-assets/icon/derived}
mkdir -p "$out/hicolor" "$out/icon.iconset"
resize() { magick "$src" -filter Lanczos -resize "${1}x${1}" -strip -define png:exclude-chunk=date,time "$2"; }
# Linux hicolor
for s in 16 24 32 48 64 128 256 512 1024; do
  mkdir -p "$out/hicolor/${s}x${s}/apps"; resize "$s" "$out/hicolor/${s}x${s}/apps/oats-meetings-rs.png"
done
# macOS iconset inputs (names iconutil expects)
for s in 16 32 128 256 512; do
  resize "$s" "$out/icon.iconset/icon_${s}x${s}.png"
  resize "$((s*2))" "$out/icon.iconset/icon_${s}x${s}@2x.png"
done
# Windows ico: PNG-compressed frames
for s in 16 24 32 48 64 128 256; do resize "$s" "$out/w$s.png"; done
magick "$out"/w16.png "$out"/w24.png "$out"/w32.png "$out"/w48.png "$out"/w64.png "$out"/w128.png "$out"/w256.png "$out/oats-meetings-rs.ico"
rm -f "$out"/w*.png
find "$out" -type f | sort | xargs sha256sum > "$out/MANIFEST.sha256"
file "$out/oats-meetings-rs.ico" 2>/dev/null || true
echo "icons.sh: ok"

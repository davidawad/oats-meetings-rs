#!/usr/bin/env bash
# Linux packages in a pod: .deb (dpkg-deb) and AppImage. Outputs in dist/.
# bin/pod-run <repo> <ref> bash ci/package-linux.sh   (LOCKED=1 adds --locked once Cargo.lock exists)
set -euo pipefail
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
# Reuse linux-nix.sh's environment (system libs, toolchain) without its build steps.
SETUP_ONLY=1 source ci/linux-nix.sh
set -x
cargo build --release -p oats-meetings-rs ${LOCKED:+--locked}
bin=target/release/oats-meetings-rs
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
bash ci/icons.sh
icons=assets/icon/derived/hicolor
mkdir -p dist
nixp() { nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#$1" | head -1; }
dpkg_out=$(nixp dpkg)
ait=$(nixp appimagetool)
export PATH="$dpkg_out/bin:$ait/bin:$PATH"
case "$(uname -m)" in x86_64) debarch=amd64 ;; aarch64) debarch=arm64 ;; *) debarch=$(uname -m) ;; esac

# --- .deb
d=$(mktemp -d)/pkg
install -Dm755 "$bin" "$d/usr/bin/oats-meetings-rs"
install -Dm644 packaging/linux/oats-meetings-rs.desktop "$d/usr/share/applications/oats-meetings-rs.desktop"
mkdir -p "$d/usr/share/icons" && cp -r "$icons" "$d/usr/share/icons/hicolor"
install -Dm644 assets/icon/LICENSE-icon "$d/usr/share/doc/oats-meetings-rs/LICENSE-icon"
mkdir -p "$d/DEBIAN"
cat > "$d/DEBIAN/control" << CTL
Package: oats-meetings-rs
Version: $ver
Architecture: $debarch
Maintainer: David Awad <angrybohr@proton.me>
Description: Oats Meetings (GPUI rewrite)
Section: sound
Priority: optional
CTL
deb="dist/oats-meetings-rs_${ver}_${debarch}.deb"
dpkg-deb --root-owner-group --build "$d" "$deb"
dpkg-deb -f "$deb"
dpkg-deb -c "$deb" | grep -q 'usr/share/icons/hicolor/256x256/apps/oats-meetings-rs.png'
dpkg-deb -x "$deb" "$d.x"
grep -q '^Icon=oats-meetings-rs$' "$d.x/usr/share/applications/oats-meetings-rs.desktop"

# --- AppImage
a=$(mktemp -d)/AppDir
install -Dm755 "$bin" "$a/usr/bin/oats-meetings-rs"
install -Dm644 packaging/linux/oats-meetings-rs.desktop "$a/oats-meetings-rs.desktop"
install -Dm644 "$icons/256x256/apps/oats-meetings-rs.png" "$a/oats-meetings-rs.png"
mkdir -p "$a/usr/share/icons" && cp -r "$icons" "$a/usr/share/icons/hicolor"
ln -s usr/bin/oats-meetings-rs "$a/AppRun"
ARCH=$(uname -m) appimagetool --appimage-extract-and-run "$a" "dist/oats-meetings-rs-$(uname -m).AppImage"
(cd dist && sha256sum ./* > SHA256SUMS && cat SHA256SUMS)

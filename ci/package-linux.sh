#!/usr/bin/env bash
# Linux packages in a pod: .deb (dpkg-deb) and AppImage. Outputs in dist/.
# bin/pod-run <repo> <ref> bash ci/package-linux.sh   (LOCKED=1 adds --locked once Cargo.lock exists)
set -euo pipefail
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
# Build in a subshell with linux-nix.sh's environment (system libs, toolchain, LD_LIBRARY_PATH). Packaging
# runs afterwards in the clean environment: nix libs on LD_LIBRARY_PATH break the host bash and dpkg tools.
(
  SETUP_ONLY=1 source ci/linux-nix.sh
  set -x
  cargo build --release -p oats-meetings-rs ${LOCKED:+--locked}
)
set -x
bin=target/release/oats-meetings-rs
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
bash ci/icons.sh
icons=assets/icon/derived/hicolor
mkdir -p dist
nixp() { nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#$1" | head -1; }
dpkg_out=$(nixp dpkg); sq=$(nixp squashfsTools)
export PATH="$dpkg_out/bin:$sq/bin:$PATH"
case "$(uname -m)" in x86_64) debarch=amd64 ;; aarch64) debarch=arm64 ;; *) debarch=$(uname -m) ;; esac

# --- .deb
d=$(mktemp -d)/pkg
install -Dm755 "$bin" "$d/usr/bin/oats-meetings-rs"
install -Dm644 packaging/linux/oats-meetings-rs.desktop "$d/usr/share/applications/oats-meetings-rs.desktop"
mkdir -p "$d/usr/share/icons" && cp -r "$icons" "$d/usr/share/icons/hicolor"
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
listing=$(dpkg-deb -c "$deb"); echo "$listing"
grep -q 'usr/share/icons/hicolor/256x256/apps/oats-meetings-rs.png' <<< "$listing"
dpkg-deb -x "$deb" "$d.x"
grep -q '^Icon=oats-meetings-rs$' "$d.x/usr/share/applications/oats-meetings-rs.desktop"

# --- AppImage
a=$(mktemp -d)/AppDir
install -Dm755 "$bin" "$a/usr/bin/oats-meetings-rs"
install -Dm644 packaging/linux/oats-meetings-rs.desktop "$a/oats-meetings-rs.desktop"
install -Dm644 "$icons/256x256/apps/oats-meetings-rs.png" "$a/oats-meetings-rs.png"
mkdir -p "$a/usr/share/icons" && cp -r "$icons" "$a/usr/share/icons/hicolor"
ln -s usr/bin/oats-meetings-rs "$a/AppRun"
# nixpkgs has no appimagetool: an AppImage is the pinned type2 runtime + a squashfs of AppDir.
case "$(uname -m)" in
  x86_64) rt_sha=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d ;;
  aarch64) rt_sha=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444 ;;
esac
rt=$(mktemp)
curl -fsSL -o "$rt" "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-$(uname -m)"
[ "$(sha256sum "$rt" | cut -d' ' -f1)" = "$rt_sha" ]
mksquashfs "$a" "$a.sqfs" -root-owned -noappend -comp zstd -no-progress
img="dist/oats-meetings-rs-$(uname -m).AppImage"
cat "$rt" "$a.sqfs" > "$img"; chmod +x "$img"
# Proof: the squashfs unpacks and carries the desktop file and icon.
off=$(stat -c %s "$rt")
sqlist=$(unsquashfs -o "$off" -l "$img"); grep -E 'oats-meetings-rs\.(desktop|png)$' <<< "$sqlist"
dtop=$(unsquashfs -o "$off" -cat "$img" oats-meetings-rs.desktop); grep -q '^Icon=oats-meetings-rs$' <<< "$dtop"
(cd dist && sha256sum ./* > SHA256SUMS && cat SHA256SUMS)

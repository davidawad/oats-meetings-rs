#!/usr/bin/env bash
# Windows cross-build in a Linux pod: linked MSVC .exe via cargo-xwin (icon embedded by build.rs),
# an .msi via wixl (msitools), static checks, and a wine run (wine only; real Windows is ci/windows-vm-run.sh).
# bin/pod-run <repo> <ref> bash ci/package-windows.sh   (LOCKED=1 adds --locked)
set -euo pipefail
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
nixp() { nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#$1" | head -1; }
set -x
bash ci/icons.sh
for p in cargo-xwin llvmPackages.lld llvmPackages.llvm msitools; do PATH="$(nixp "$p")/bin:$PATH"; done
export PATH
rustup target add x86_64-pc-windows-msvc
export XWIN_ACCEPT_LICENSE=1 XWIN_CACHE_DIR="${XWIN_CACHE_DIR:-$HOME/.xwin-cache}"
cargo xwin build --release --target x86_64-pc-windows-msvc -p oats-meetings-rs ${LOCKED:+--locked}
exe=target/x86_64-pc-windows-msvc/release/oats-meetings-rs.exe
mkdir -p dist && cp "$exe" dist/
file dist/oats-meetings-rs.exe
# icon resource present in the PE: .rsrc section holds an RT_GROUP_ICON
llvm-objdump -h dist/oats-meetings-rs.exe | grep -E '\.rsrc'
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OATS_VERSION="$ver" OATS_EXE="$PWD/dist/oats-meetings-rs.exe" OATS_ICO="$PWD/assets/icon/derived/oats-meetings-rs.ico" \
  wixl -v -a x64 -o "dist/oats-meetings-rs-$ver-x64.msi" packaging/windows/oats-meetings-rs.wxs
msiinfo suminfo "dist/oats-meetings-rs-$ver-x64.msi"
msiinfo tables "dist/oats-meetings-rs-$ver-x64.msi"
(cd dist && sha256sum ./* > SHA256SUMS && cat SHA256SUMS)

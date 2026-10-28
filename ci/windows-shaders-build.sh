#!/usr/bin/env bash
# Cross-build tools/shadergen to dist/oats-shadergen.exe (static CRT) with cargo-xwin. Pod only.
set -euo pipefail
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
for p in cargo-xwin llvmPackages.lld llvmPackages.llvm; do for o in $(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#$p"); do PATH="$o/bin:$PATH"; done; done
export PATH XWIN_ACCEPT_LICENSE=1 XWIN_CACHE_DIR="${XWIN_CACHE_DIR:-$HOME/.xwin-cache}"
export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static"
rustup target add x86_64-pc-windows-msvc
(cd tools/shadergen && cargo xwin build --release --target x86_64-pc-windows-msvc)
mkdir -p dist
cp tools/shadergen/target/x86_64-pc-windows-msvc/release/oats-shadergen.exe dist/oats-shadergen.exe

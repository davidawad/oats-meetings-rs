#!/usr/bin/env bash
# Linux build+test inside a rootless pod: system deps come from nixpkgs (no apt, no root).
set -euo pipefail
# Pinned nixpkgs (nixos-unstable on 2026-10-27) for reproducible system libs.
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
pkgs=(libxkbcommon wayland libxcb libx11 fontconfig freetype vulkan-loader alsa-lib openssl zstd pkg-config cmake)
paths=""
pc=""
ld=""
for p in "${pkgs[@]}"; do
  for out in $(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#$p" "$NIXPKGS#$p.dev" 2> /dev/null | sort -u); do
    paths="$paths:$out/bin"
    [ -d "$out/lib/pkgconfig" ] && pc="$pc:$out/lib/pkgconfig"
    [ -d "$out/share/pkgconfig" ] && pc="$pc:$out/share/pkgconfig"
    [ -d "$out/lib" ] && ld="$ld:$out/lib"
  done
done
export PATH="${PATH}${paths}"
export PKG_CONFIG_PATH="${pc#:}"
export LD_LIBRARY_PATH="${ld#:}"
export RUSTFLAGS="${RUSTFLAGS:-} $(echo "${ld#:}" | tr ':' '\n' | sed 's/^/-L native=/' | tr '\n' ' ')"
set -x
cargo build --workspace
cargo test --workspace
cargo run -q -- --headless --version
cargo build -p oats-meetings-rs --no-default-features
cargo run -q --no-default-features -- --headless --version

#!/usr/bin/env bash
# Linux build+test inside a rootless pod: system deps come from nixpkgs (no apt, no root).
set -euo pipefail
# Pinned nixpkgs (nixos-unstable on 2026-10-27) for reproducible system libs.
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
pkgs=(libxkbcommon wayland libxcb libx11 fontconfig freetype vulkan-loader alsa-lib openssl zstd pkg-config cmake zlib bzip2 libpng brotli expat libxau libxdmcp xorgproto libxkbcommon libxcb)
paths=""
pc=""
ld=""
roots=""
for p in "${pkgs[@]}"; do
  for o in "$p" "$p.dev"; do
    roots="$roots $(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#$o" 2> /dev/null | tr '\n' ' ' || true)"
  done
done
# pkg-config follows Requires: into transitive -dev outputs (freetype2 -> zlib, bzip2, libpng, brotli), so walk the closure.
# shellcheck disable=SC2086
for out in $(nix "${nixf[@]}" path-info -r $roots | sort -u); do
  # The host libc must stay the host libc: nix glibc/gcc in LD_LIBRARY_PATH breaks every host binary.
  case "$out" in *-glibc-*|*-gcc-*|*-libgcc*|*-binutils*|*-bash-*) continue ;; esac
  [ -d "$out/bin" ] && paths="$paths:$out/bin"
  [ -d "$out/lib/pkgconfig" ] && pc="$pc:$out/lib/pkgconfig"
  [ -d "$out/share/pkgconfig" ] && pc="$pc:$out/share/pkgconfig"
  [ -d "$out/lib" ] && ld="$ld:$out/lib"
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

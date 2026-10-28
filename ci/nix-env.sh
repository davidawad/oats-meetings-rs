#!/usr/bin/env bash
# Sourced by the other ci scripts: puts the pinned nix GUI libs and a toolchain on the environment.
# Linux build+test inside a rootless pod: system deps come from nixpkgs (no apt, no root).
# Pinned nixpkgs (nixos-unstable on 2026-10-27) for reproducible system libs.
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
pkgs=(llvmPackages.llvm llvmPackages.libclang llvmPackages.libclang.lib libxkbcommon wayland libxcb libx11 fontconfig freetype vulkan-loader alsa-lib openssl zstd pkg-config cmake zlib bzip2 libpng brotli expat libxau libxdmcp xorgproto libxkbcommon libxcb)
paths=""
# Bare nix images (nixos/nix) have no Rust or C toolchain: provide them. Pods already have both.
if ! command -v cc > /dev/null; then
  gcc_out=$(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#gcc" | head -1)
  export PATH="$gcc_out/bin:$PATH"
fi
if ! command -v cargo > /dev/null; then
  rustup_out=$(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#rustup" | head -1)
  export PATH="$rustup_out/bin:$PATH"
  rustup toolchain install stable --profile minimal
fi
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
# bindgen (whisper-rs-sys): libclang.so is found through LD_LIBRARY_PATH; its resource headers
# (stddef.h, ...) come from the clang package and are passed explicitly.
for d in $roots; do
  for inc in "$d"/lib/clang/*/include; do
    if [ -d "$inc" ]; then
      export BINDGEN_EXTRA_CLANG_ARGS="${BINDGEN_EXTRA_CLANG_ARGS:-} -isystem $inc"
    fi
  done
done
true

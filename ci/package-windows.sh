#!/usr/bin/env bash
# Windows cross-build in a Linux pod: linked MSVC .exe via cargo-xwin (icon embedded by build.rs),
# an .msi via wixl (msitools) and static checks of both.
# bin/pod-run <repo> <ref> bash ci/package-windows.sh [headless|gui|gui-debug]   (LOCKED=1 adds --locked)
# headless (default): release exe without the gpui feature; the msi ships this.
# gui-debug: link-only proof of the GUI build. gpui_windows compiles its HLSL with fxc.exe in a build script gated on
#   the HOST os, so a Linux-hosted RELEASE gui build has no embedded shaders and fails to compile (include! of
#   shaders_bytes.rs). The debug profile compiles shaders at runtime instead, so it links, but is not shippable.
set -euo pipefail
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
nixp() { nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#$1" | head -1; }
variant=${1:-headless}
set -x
bash ci/icons.sh
for p in cargo-xwin llvmPackages.lld llvmPackages.llvm msitools; do PATH="$(nixp "$p")/bin:$PATH"; done
export PATH
rustup target add x86_64-pc-windows-msvc
# Static CRT: the exe must not need the VC++ redistributable (absent on a fresh Windows).
export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static"
export XWIN_ACCEPT_LICENSE=1 XWIN_CACHE_DIR="${XWIN_CACHE_DIR:-$HOME/.xwin-cache}"
case "$variant" in
  headless)
    cargo xwin build --release --target x86_64-pc-windows-msvc -p oats-meetings-rs --no-default-features ${LOCKED:+--locked}
    exe=target/x86_64-pc-windows-msvc/release/oats-meetings-rs.exe
    ;;
  gui)
    # Release GUI. gpui_windows' build script only compiles shaders when the HOST is Windows, so swap it (inside this
    # throwaway pod's cargo checkout, no lockfile change) for one that installs the hash-pinned shaders generated in a
    # real Windows VM (assets/windows-shaders, see its README).
    cargo fetch ${LOCKED:+--locked}
    gw=$(dirname "$(find "${CARGO_HOME:-/usr/local/cargo}" "$HOME/.cargo" -path '*gpui_windows/src/shaders.hlsl' 2> /dev/null | head -1)")/..
    (cd "$gw/src" && sha256sum -c "$OLDPWD/assets/windows-shaders/sources.sha256")
    sha256sum -c assets/windows-shaders/shaders_bytes.sha256 --ignore-missing 2> /dev/null || (cd assets/windows-shaders && sha256sum -c shaders_bytes.sha256)
    cat > "$gw/build.rs" << 'RS'
fn main() {
    println!("cargo:rerun-if-env-changed=OATS_SHADERS_BYTES");
    let out = std::env::var("OUT_DIR").unwrap();
    let src = std::env::var("OATS_SHADERS_BYTES").expect("OATS_SHADERS_BYTES");
    std::fs::copy(src, format!("{out}/shaders_bytes.rs")).unwrap();
}
RS
    export OATS_SHADERS_BYTES="$PWD/assets/windows-shaders/shaders_bytes.rs"
    cargo xwin build --release --target x86_64-pc-windows-msvc -p oats-meetings-rs ${LOCKED:+--locked}
    exe=target/x86_64-pc-windows-msvc/release/oats-meetings-rs.exe
    ;;
  gui-debug)
    cargo xwin build --target x86_64-pc-windows-msvc -p oats-meetings-rs ${LOCKED:+--locked}
    exe=target/x86_64-pc-windows-msvc/debug/oats-meetings-rs.exe
    ;;
  *)
    echo "unknown variant $variant" >&2
    exit 2
    ;;
esac
mkdir -p dist && cp "$exe" dist/oats-meetings-rs.exe
llvm-readobj --file-headers dist/oats-meetings-rs.exe | grep -E "Machine|Subsystem|Characteristics"
# icon resource present in the PE: a .rsrc section exists and holds RT_GROUP_ICON/RT_ICON entries
llvm-objdump -h dist/oats-meetings-rs.exe | grep -E '\.rsrc'
llvm-readobj --coff-resources dist/oats-meetings-rs.exe | grep -E 'ICON|VERSION' || true
imports=$(llvm-readobj --coff-imports dist/oats-meetings-rs.exe | grep -i "Name:" | sort -u); echo "$imports"
if grep -qi -E "vcruntime|msvcp|ucrtbase" <<< "$imports"; then echo "exe imports a CRT DLL" >&2; exit 1; fi
ver=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
msi="dist/oats-meetings-rs-$ver-x64.msi"
OATS_VERSION="$ver" OATS_EXE="$PWD/dist/oats-meetings-rs.exe" OATS_ICO="$PWD/assets/icon/derived/oats-meetings-rs.ico" \
  wixl -v -a x64 -o "$msi" packaging/windows/oats-meetings-rs.wxs
msiinfo suminfo "$msi"
msiinfo tables "$msi"
(cd dist && sha256sum ./* > SHA256SUMS && cat SHA256SUMS)
# msi contents: extract and compare against the inputs (msiextract is read-only unpacking of the msi tables + cab)
x=$(mktemp -d)
msiextract -C "$x" "$msi"
find "$x" -type f
cmp "$x"/*/*/oats-meetings-rs.exe dist/oats-meetings-rs.exe 2>/dev/null || cmp "$(find "$x" -name oats-meetings-rs.exe | head -1)" dist/oats-meetings-rs.exe
find "$x" -name oats-meetings-rs.ico | head -1 | xargs -I{} cmp {} assets/icon/derived/oats-meetings-rs.ico
# WINE ONLY (not real Windows): smoke run of the exe. The real-Windows proof is separate (dot-4uzgol.16 VM step).
if [ -z "${NO_WINE:-}" ]; then
  PATH="$(nixp wine64)/bin:$PATH"
  export WINEPREFIX="$(mktemp -d)/pfx" WINEDEBUG=-all WINEARCH=win64
  wine dist/oats-meetings-rs.exe --headless --version; echo "WINE_RC=$?"
fi

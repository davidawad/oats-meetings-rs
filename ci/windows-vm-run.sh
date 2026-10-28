#!/usr/bin/env bash
# Real-Windows run proof in a Linux pod: unattended install of the free Windows Server 2022 Evaluation (Server Core)
# under QEMU, then run oats-meetings-rs.exe --headless --version inside it and read the exit code over COM1.
# Pods expose no /dev/kvm, so QEMU uses TCG (software emulation): slow (hours), but a real Windows kernel.
# Modes: fresh install (default) or cached base. VM_CACHE=<dir> (a PVC mount): win.iso and base.qcow2 live there;
# a missing base.qcow2 is installed once and kept, every run then boots a throwaway overlay of it. KVM is used when
# /dev/kvm exists (cpu host), else TCG.
# bin/pod-run <repo> <ref> bash ci/windows-vm-run.sh        (log lines OATS_* are the proof)
set -euo pipefail
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
nixp() { nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#$1" | head -1; }
ISO_URL=${ISO_URL:-'https://go.microsoft.com/fwlink/p/?LinkID=2195280&clcid=0x409&culture=en-us&country=US'}
TIMEOUT_S=${TIMEOUT_S:-28800}
echo "OATS_VM kvm=$([ -e /dev/kvm ] && echo yes || echo no) cpus=$(nproc)"
# TASK=version (default): run oats-meetings-rs.exe --headless --version.
# TASK=shaders: run tools/shadergen (real d3dcompiler_47) over gpui_windows' HLSL and return shaders_bytes.rs
#   base64 between OATS_B64_BEGIN/OATS_B64_END lines, with its SHA256 as computed inside Windows.
TASK=${TASK:-version}
# 1. the payload exe, built without wine
if [ "$TASK" = shaders ]; then
  cargo fetch --locked
  bash ci/windows-shaders-build.sh
  exe=$PWD/dist/oats-shadergen.exe
  hlsl_dir=$(dirname "$(find "${CARGO_HOME:-/usr/local/cargo}" "$HOME/.cargo" -path '*gpui_windows/src/shaders.hlsl' 2> /dev/null | head -1)")
  [ -f "$hlsl_dir/shaders.hlsl" ] || {
    echo "no gpui_windows HLSL sources found" >&2
    exit 1
  }
else
  NO_WINE=1 bash ci/package-windows.sh "${VARIANT:-headless}"
  exe=$PWD/dist/oats-meetings-rs.exe
fi
sha256sum "$exe"
for p in qemu libisoburn; do for o in $(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#$p"); do PATH="$o/bin:$PATH"; done; done
export PATH
repo=$PWD
w=$(mktemp -d -p "${WORK_DIR:-/work}")
cd "$w"
# 2. Windows Server 2022 Evaluation ISO (free from Microsoft; accepting the evaluation EULA in the unattend file)
cache=${VM_CACHE:-}
if [ -n "$cache" ] && [ -s "$cache/win.iso" ]; then iso=$cache/win.iso; else
  curl -fsSL --retry 5 -o win.iso "$ISO_URL"
  iso=$PWD/win.iso
  [ -z "$cache" ] || cp win.iso "$cache/win.iso"
fi
sha256sum "$iso"
# 3. config CD: autounattend.xml + the exe + the run script
mkdir cfg
# IMAGE_INDEX: 1 = Server Core (default; no dcomp.dll, so the GUI exe cannot load), 2 = Desktop Experience.
idx=${IMAGE_INDEX:-$([ "${VARIANT:-}" = gui ] && echo 2 || echo 1)}
sed "s|<Key>/IMAGE/INDEX</Key><Value>1</Value>|<Key>/IMAGE/INDEX</Key><Value>$idx</Value>|" "$repo/ci/windows-vm/autounattend.xml" > cfg/autounattend.xml
echo "OATS_VM image_index=$idx"
if [ "$TASK" = shaders ]; then
  cp "$exe" cfg/oats-shadergen.exe
  cp "$hlsl_dir"/*.hlsl cfg/
  cp "$repo/ci/windows-vm/oats-shaders.cmd" cfg/oats-run.cmd
else
  cp "$exe" cfg/oats-meetings-rs.exe
  llvm_out=$(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#llvmPackages.llvm" | head -1)
  "$llvm_out/bin/llvm-readobj" --coff-imports "$exe" | awk '/Name:/{print $2}' | sort -u > cfg/dlls.txt
  cat cfg/dlls.txt
  cp "$repo/ci/windows-vm/oats-run.cmd" cfg/oats-run.cmd
fi
xorriso -as mkisofs -o cfg.iso -J -R -V OATSCFG cfg
if [ -n "$cache" ] && [ -s "$cache/base-$idx.qcow2" ]; then
  echo "OATS_VM mode=overlay-of-cached-base"
  qemu-img create -f qcow2 -b "$cache/base-$idx.qcow2" -F qcow2 disk.qcow2
  installing=0
else
  echo "OATS_VM mode=fresh-install"
  qemu-img create -f qcow2 disk.qcow2 40G
  installing=1
fi
if [ -e /dev/kvm ] && [ -w /dev/kvm ]; then accel=(-accel kvm -cpu host); else accel=(-accel tcg,thread=multi -cpu max); fi
echo "OATS_VM accel=${accel[1]}"
# 4. boot: install, first logon runs oats-run.cmd, which reports over COM1 and powers off
set +e
timeout -k 60 "$TIMEOUT_S" qemu-system-x86_64 \
  "${accel[@]}" -smp 4 -m 8G \
  -machine pc -display none -vga std -nic none \
  -drive if=none,id=hd,file=disk.qcow2,format=qcow2 -device ide-hd,drive=hd,bus=ide.0,unit=0,bootindex=2 \
  -drive if=none,id=win,file="$iso",media=cdrom,readonly=on -device ide-cd,drive=win,bus=ide.1,unit=0,bootindex=1 \
  -drive if=none,id=cfg,file=cfg.iso,media=cdrom,readonly=on -device ide-cd,drive=cfg,bus=ide.1,unit=1 \
  -serial file:serial.log -monitor none &
q=$!
# tail the serial log into the job log until qemu exits
(while kill -0 $q 2> /dev/null; do
  sleep 60
  echo "OATS_VM alive $(date -u +%T) serial_bytes=$(stat -c %s serial.log 2> /dev/null || echo 0)"
done) &
wait $q
echo "OATS_VM qemu_rc=$?"
set -e
if [ "$TASK" = shaders ]; then
  # keep the log small for a reader: everything except the base64 body is printed by the log itself
  sed -n '/OATS_B64_BEGIN/,/OATS_B64_END/p' serial.log | tr -d '\r'
  grep -a -v -E '^[A-Za-z0-9+/=]{40,}\r?$' serial.log | tr -d '\r' || true
else
  cat serial.log || true
fi
grep -a 'OATS_RC=' serial.log
grep -a -q 'OATS_RC=0' serial.log
[ "$TASK" != version ] || grep -a -q '^oats-meetings-rs [0-9]' serial.log
# keep the installed disk as the cached base (it has run once; later runs overlay it and re-run via the OatsRun task)
if [ "$installing" = 1 ] && [ -n "$cache" ]; then
  qemu-img convert -O qcow2 disk.qcow2 "$cache/base-$idx.qcow2.tmp" && mv "$cache/base-$idx.qcow2.tmp" "$cache/base-$idx.qcow2"
  echo "OATS_VM base cached"
fi

#!/usr/bin/env bash
# Real-Windows run proof in a Linux pod: unattended install of the free Windows Server 2022 Evaluation (Server Core)
# under QEMU, then run oats-meetings-rs.exe --headless --version inside it and read the exit code over COM1.
# Pods expose no /dev/kvm, so QEMU uses TCG (software emulation): slow (hours), but a real Windows kernel.
# bin/pod-run <repo> <ref> bash ci/windows-vm-run.sh        (log lines OATS_* are the proof)
set -euo pipefail
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
nixp() { nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#$1" | head -1; }
ISO_URL=${ISO_URL:-'https://go.microsoft.com/fwlink/p/?LinkID=2195280&clcid=0x409&culture=en-us&country=US'}
TIMEOUT_S=${TIMEOUT_S:-28800}
echo "OATS_VM kvm=$([ -e /dev/kvm ] && echo yes || echo no) cpus=$(nproc)"
# 1. the exe (headless release, static CRT), no wine here
NO_WINE=1 bash ci/package-windows.sh headless
exe=$PWD/dist/oats-meetings-rs.exe
sha256sum "$exe"
for p in qemu libisoburn; do for o in $(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#$p"); do PATH="$o/bin:$PATH"; done; done
export PATH
repo=$PWD
w=$(mktemp -d -p "${WORK_DIR:-/work}")
cd "$w"
# 2. Windows Server 2022 Evaluation ISO (free from Microsoft; accepting the evaluation EULA in the unattend file)
curl -fsSL --retry 5 -o win.iso "$ISO_URL"
sha256sum win.iso
# 3. config CD: autounattend.xml + the exe + the run script
mkdir cfg
cp "$exe" cfg/oats-meetings-rs.exe
cp "$repo/ci/windows-vm/autounattend.xml" cfg/autounattend.xml
cp "$repo/ci/windows-vm/oats-run.cmd" cfg/oats-run.cmd
xorriso -as mkisofs -o cfg.iso -J -R -V OATSCFG cfg
qemu-img create -f qcow2 disk.qcow2 40G
# 4. boot: install, first logon runs oats-run.cmd, which reports over COM1 and powers off
set +e
timeout -k 60 "$TIMEOUT_S" qemu-system-x86_64 \
  -accel tcg,thread=multi -cpu max -smp 4 -m 8G \
  -machine pc -display none -vga std -nic none \
  -drive if=none,id=hd,file=disk.qcow2,format=qcow2 -device ide-hd,drive=hd,bootindex=2 \
  -drive if=none,id=win,file=win.iso,media=cdrom,readonly=on -device ide-cd,drive=win,bootindex=1 \
  -drive if=none,id=cfg,file=cfg.iso,media=cdrom,readonly=on -device ide-cd,drive=cfg \
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
cat serial.log || true
grep -a 'OATS_RC=' serial.log
grep -a -q 'OATS_RC=0' serial.log

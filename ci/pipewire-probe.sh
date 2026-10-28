#!/usr/bin/env bash
# Diagnostic (dot-4uzgol.7): can PipeWire + pipewire-pulse come up in a rootless pod?
set -uxo pipefail
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
nixf=(--extra-experimental-features "nix-command flakes")
pw=$(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#pipewire" | head -1)
pa=$(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#pulseaudio" | head -1)
wp=$(nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#wireplumber" | head -1)
echo "pw=$pw pa=$pa wp=$wp"
ls "$pw/bin" "$wp/bin"
id
ls -la /dev/snd /run/dbus 2>&1 | head
export PATH="$pw/bin:$wp/bin:$pa/bin:$PATH"
export XDG_RUNTIME_DIR
XDG_RUNTIME_DIR=$(mktemp -d)
chmod 700 "$XDG_RUNTIME_DIR"
export HOME=$XDG_RUNTIME_DIR XDG_CONFIG_HOME=$XDG_RUNTIME_DIR/cfg XDG_STATE_HOME=$XDG_RUNTIME_DIR/state XDG_DATA_HOME=$XDG_RUNTIME_DIR/data
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME"
export PIPEWIRE_DEBUG=3
pipewire > "$XDG_RUNTIME_DIR/pw.log" 2>&1 &
sleep 3
ls -la "$XDG_RUNTIME_DIR"
tail -30 "$XDG_RUNTIME_DIR/pw.log"
pw-cli info 0 2>&1 | head -5
wireplumber > "$XDG_RUNTIME_DIR/wp.log" 2>&1 &
sleep 3
tail -20 "$XDG_RUNTIME_DIR/wp.log"
pipewire-pulse > "$XDG_RUNTIME_DIR/pwp.log" 2>&1 &
sleep 3
ls -la "$XDG_RUNTIME_DIR" "$XDG_RUNTIME_DIR/pulse" 2>&1
tail -30 "$XDG_RUNTIME_DIR/pwp.log"
export PULSE_SERVER="unix:$XDG_RUNTIME_DIR/pulse/native"
pactl info 2>&1 | head -8
pactl load-module module-null-sink sink_name=probe 2>&1
pw-dump 2>&1 | head -c 600

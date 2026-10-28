#!/usr/bin/env bash
# Diagnostic (dot-4uzgol.7): does a rootless Pulse null-sink monitor run in real time in a pod?
set -uo pipefail
NIXPKGS=github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504
paudio=$(nix --extra-experimental-features "nix-command flakes" build --no-link --print-out-paths "$NIXPKGS#pulseaudio" | head -1)
export PATH="$paudio/bin:$PATH"
id; nproc; cat /sys/fs/cgroup/cpu.max 2> /dev/null || true
export XDG_RUNTIME_DIR; XDG_RUNTIME_DIR=$(mktemp -d); chmod 700 "$XDG_RUNTIME_DIR"
export PULSE_RUNTIME_PATH="$XDG_RUNTIME_DIR/pulse" PULSE_STATE_PATH="$XDG_RUNTIME_DIR/st" PULSE_CONFIG_PATH="$XDG_RUNTIME_DIR/cfg" HOME="$XDG_RUNTIME_DIR"
mkdir -p "$PULSE_RUNTIME_PATH" "$PULSE_STATE_PATH" "$PULSE_CONFIG_PATH"
export PULSE_SERVER="unix:$PULSE_RUNTIME_PATH/native"
pulseaudio -n --daemonize=yes --exit-idle-time=-1 --log-target="file:$XDG_RUNTIME_DIR/p.log" -L "module-native-protocol-unix auth-anonymous=1 socket=$PULSE_RUNTIME_PATH/native"
for i in $(seq 1 30); do pactl info > /dev/null 2>&1 && break; sleep 0.5; done
probe() { # name, sink module args, feeder (zero|none|paced), parec extra args
  local name=$1 args=$2 feed=$3 extra=${4:-}
  local id; id=$(pactl load-module module-null-sink "sink_name=$name $args")
  local tp=""
  case $feed in
    zero) pacat --playback --device="$name" --format=float32le --rate=48000 --channels=1 < /dev/zero & tp=$! ;;
    paced) (while :; do head -c 38400 /dev/zero; sleep 0.1; done) | pacat --playback --device="$name" --format=float32le --rate=48000 --channels=1 & tp=$! ;;
  esac
  sleep 0.5
  local n; n=$(timeout 5 parec --device="$name.monitor" --format=float32le --rate=48000 --channels=1 $extra | wc -c)
  [ -z "$tp" ] || kill "$tp" 2> /dev/null
  echo "PROBE $name feed=$feed args='$args' extra='$extra' bytes=$n (expect 960000)"
  pactl unload-module "$id"
}
probe a1 "" none
probe a2 "" zero
probe a3 "" paced
probe b1 "rate=48000 channels=1" none
probe b2 "rate=48000 channels=1" zero
probe b3 "rate=48000 channels=1" paced
probe c1 "rate=48000 channels=1" none "--latency-msec=100"
probe c2 "rate=48000 channels=1" zero "--latency-msec=100"
probe d1 "rate=48000 channels=1 format=float32le" zero
tail -5 "$XDG_RUNTIME_DIR/p.log"

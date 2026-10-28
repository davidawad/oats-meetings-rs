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
ttfb() { local t0 t1; t0=$(date +%s%N); parec --device="$1.monitor" --format=float32le --rate=48000 --channels=1 --latency-msec=100 | head -c 4 > /dev/null; t1=$(date +%s%N); echo "PROBE ttfb $1 = $(( (t1 - t0) / 1000000 )) ms"; }
id=$(pactl load-module module-null-sink sink_name=t1 rate=48000 channels=1)
ttfb t1; ttfb t1
for lat in 20 50 100 200 500; do
  t0=$(date +%s%N)
  n=$(timeout 10 parec --device=t1.monitor --format=float32le --rate=48000 --channels=1 --latency-msec=$lat | wc -c)
  t1=$(date +%s%N)
  echo "PROBE lat=$lat bytes=$n wall_ms=$(( (t1 - t0) / 1000000 )) audio_ms=$(( n / 192 )) (10s run)"
done
# What does the pa clock do: sample the monitor latency
for k in 1 2 3; do pactl list sources short | head -3; sleep 1; done
top -b -n1 | head -15
tail -5 "$XDG_RUNTIME_DIR/p.log"

#!/usr/bin/env bash
# Linux audio proof in a rootless pod (dot-4uzgol.7): a Pulse server with two null sinks, a tone
# played into each, and the real binary recording through `ctl record start|stop`.
#   bin/pod-run <repo> <ref> bash ci/audio-linux.sh
# PipeWire too: AUDIO_PIPEWIRE=1 starts pipewire + wireplumber + pipewire-pulse instead of pulseaudio.
set -euo pipefail
source "$(dirname "$0")/nix-env.sh"
paudio=$(env -u LD_LIBRARY_PATH nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#pulseaudio" | head -1)
export PATH="$paudio/bin:$PATH"
id
export XDG_RUNTIME_DIR; XDG_RUNTIME_DIR=$(mktemp -d); chmod 700 "$XDG_RUNTIME_DIR"
export PULSE_RUNTIME_PATH="$XDG_RUNTIME_DIR/pulse" PULSE_STATE_PATH="$XDG_RUNTIME_DIR/pulse-state" PULSE_CONFIG_PATH="$XDG_RUNTIME_DIR/pulse-cfg"
mkdir -p "$PULSE_RUNTIME_PATH" "$PULSE_STATE_PATH" "$PULSE_CONFIG_PATH"
export PULSE_SERVER="unix:$PULSE_RUNTIME_PATH/native"
backends=auto,pulse
if [ -n "${AUDIO_PIPEWIRE:-}" ]; then
  # pipewire has several outputs (bin, lib, man ...): put every output's bin dir on PATH.
  for o in $(env -u LD_LIBRARY_PATH nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#pipewire"); do PATH="$o/bin:$PATH"; done
  wp=$(env -u LD_LIBRARY_PATH nix "${nixf[@]}" build --no-link --print-out-paths "$NIXPKGS#wireplumber" | head -1)
  export PATH="$wp/bin:$PATH"
  export PIPEWIRE_RUNTIME_DIR="$XDG_RUNTIME_DIR"
  env -u LD_LIBRARY_PATH pipewire > "$XDG_RUNTIME_DIR/pw.log" 2>&1 &
  sleep 2
  env -u LD_LIBRARY_PATH wireplumber > "$XDG_RUNTIME_DIR/wp.log" 2>&1 &
  sleep 2
  env -u LD_LIBRARY_PATH pipewire-pulse > "$XDG_RUNTIME_DIR/pwp.log" 2>&1 &
  sleep 2
  backends=auto,pulse,pipewire
else
  # -n: no default.pa. The daemon refuses root, so drop to nobody when we are root.
  as=()
  if [ "$(id -u)" = 0 ]; then
    chown -R 65534:65534 "$XDG_RUNTIME_DIR"
    as=(setpriv --reuid=65534 --regid=65534 --clear-groups)
  fi
  env -u LD_LIBRARY_PATH HOME="$XDG_RUNTIME_DIR" "${as[@]}" pulseaudio -n --daemonize=yes --exit-idle-time=-1 \
    --log-target="file:$XDG_RUNTIME_DIR/pulse.log" -L "module-native-protocol-unix auth-anonymous=1 socket=$PULSE_RUNTIME_PATH/native" || { cat "$XDG_RUNTIME_DIR/pulse.log" || true; exit 1; }
fi
for i in $(seq 1 30); do pactl info > /dev/null 2>&1 && break; sleep 0.5; done
pactl info > /dev/null 2>&1 || { tail -30 "$XDG_RUNTIME_DIR"/*.log 2> /dev/null; ls -la "$XDG_RUNTIME_DIR" "$PULSE_RUNTIME_PATH"; exit 1; }
pactl info | sed -n '1,8p'
pactl load-module module-null-sink sink_name=oats_sink sink_properties=device.description=OatsSystem
pactl load-module module-null-sink sink_name=oats_mic_sink sink_properties=device.description=OatsMic
pactl set-default-sink oats_sink
pactl list short sources
# Diagnostic: how many bytes parec delivers from a monitor in 5 s of wall clock (48 kHz mono f32: 960000 expected).
(pacat --playback --device=oats_sink --format=float32le --rate=48000 --channels=1 < /dev/zero & tp=$!; sleep 0.5; \
  n=$(timeout 5 parec --device=oats_sink.monitor --format=float32le --rate=48000 --channels=1 | wc -c); kill $tp; echo "parec 5s bytes=$n (expect ~960000)") || true
# Local ASR for the speech test: the checksummed model (ci/fetch-models.sh) and a whisper.cpp build (libclang
# comes from the host via ci/nix-env.sh).
export OATS_ONLY_MODEL=tiny.en
OATS_FETCH_PARAKEET=0 OATS_FETCH_MODELS=tiny.en source "$(dirname "$0")/fetch-models.sh"
set -x
cargo build --locked --workspace 
rc=0
cargo test --locked --workspace  --no-fail-fast || rc=$?
OATS_AUDIO_LIVE=1 OATS_AUDIO_BACKENDS="$backends" cargo test --locked  --test audio_live -- --nocapture --test-threads=1 || rc=$?
exit $rc

#!/usr/bin/env bash
# Cross-target check of the audio code for Windows in a Linux pod (dot-4uzgol.7): compiles oats-audio
# (cpal/WASAPI backend, the windows_loopback test), oats-core and oats-ctl for the MSVC target. A check
# only: nothing links or runs. The runtime proof is crates/audio/tests/windows_loopback.rs in the
# dot-4uzgol.16 Windows VM.   bin/pod-run <repo> <ref> bash ci/windows-check-audio.sh
set -euo pipefail
SETUP_ONLY=1 source "$(dirname "$0")/linux-nix.sh"
set -x
T=${CROSS_TARGET:-x86_64-pc-windows-msvc}
rustup target add "$T"
cargo check --locked --target "$T" -p oats-audio --all-targets
cargo check --locked --target "$T" -p oats-core -p oats-ctl --all-targets
# The cpal backend must really be in the Windows build, and not in the Linux one.
cargo tree --locked --target "$T" -p oats-audio -e normal | grep -q '^.*cpal'
! cargo tree --locked --target x86_64-unknown-linux-gnu -p oats-audio -e normal | grep -q 'cpal'

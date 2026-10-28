#!/usr/bin/env bash
# Does whisper-rs (whisper.cpp + bindgen) build in the pod? Usage: bin/pod-run <repo> <ref> bash ci/whisper-probe.sh
set -euo pipefail
source "$(dirname "$0")/nix-env.sh"
set -x
cargo build --locked -p oats-core --features whisper

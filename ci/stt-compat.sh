#!/usr/bin/env bash
# Dump the session store the LOCAL whisper pipeline writes, to prove `bin/meeting sessions` reads it.
#   pod:     bin/pod-run <repo> <ref> bash ci/stt-compat.sh       (prints the store as a tar.gz base64 block)
#   carbon:  sed -n '/^-----BEGIN-STORE/,/^-----END-STORE/p' <log> | sed '1d;$d' | base64 -d | tar xz -C <dir>
#            OPENOATS_SESSIONS_ROOT=<dir>/sessions bin/meeting sessions list
set -euo pipefail
source "$(dirname "$0")/nix-env.sh"
source "$(dirname "$0")/fetch-models.sh" >&2
d=$(mktemp -d)
OATS_STORE_DUMP="$d/sessions" cargo test --locked -p oats-core --features whisper --test stt_whisper engine_local_backend -- --nocapture >&2
echo "-----BEGIN-STORE"
tar -C "$d" -czf - sessions | base64
echo "-----END-STORE"

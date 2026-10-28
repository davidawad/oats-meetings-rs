#!/usr/bin/env bash
# Dump the store this app writes, to prove the dotfiles `bin/meeting sessions …` reads it unchanged.
#   pod:     bin/pod-run <repo> <ref> bash ci/meeting-compat.sh      (prints the store as a tar.gz base64 block)
#   carbon:  sed -n '/^-----BEGIN-STORE/,/^-----END-STORE/p' <log> | sed '1d;$d' | base64 -d | tar xz -C <dir>
#            OPENOATS_SESSIONS_ROOT=<dir>/sessions bin/meeting sessions list
set -euo pipefail
source "$(dirname "$0")/nix-env.sh"
d=$(mktemp -d)
OATS_STORE_DUMP="$d/sessions" cargo test --locked --test control_cli engine_store_for_meeting_compat -- --exact --nocapture >&2
echo "-----BEGIN-STORE"
tar -C "$d" -czf - sessions | base64
echo "-----END-STORE"

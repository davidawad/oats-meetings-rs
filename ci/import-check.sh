#!/usr/bin/env bash
# Import an OpenOats templates.json with the real binary and print what came out.
# Usage: bin/pod-run <repo> <ref> env TEMPLATES_B64=<base64 of the file> bash ci/import-check.sh
set -euo pipefail
source "$(dirname "$0")/nix-env.sh"
d=$(mktemp -d)
echo "$TEMPLATES_B64" | base64 -d > "$d/templates.json"
cargo build -q --locked --no-default-features
bin=target/debug/oats-meetings-rs
export OATS_RUNTIME_DIR="$d/rt" OATS_CONFIG="$d/config.toml" OATS_DATA_DIR="$d/data"
"$bin" --headless 2> /dev/null &
pid=$!
for _ in $(seq 100); do "$bin" ctl ping > /dev/null 2>&1 && break; sleep 0.1; done
"$bin" ctl --json template import --from "$d/templates.json" | head -c 600; echo
"$bin" ctl --json template list
"$bin" ctl quit > /dev/null
wait "$pid" || true

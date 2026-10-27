#!/usr/bin/env bash
# Regenerate Cargo.lock and config/ projections in a pod and push them to branch $1 (never main).
# Usage: bin/pod-run <repo> <ref> bash ci/regen.sh <push-branch>
set -euo pipefail
source "$(dirname "$0")/nix-env.sh"
cargo generate-lockfile
cargo run -q --no-default-features -- config generate --dir config
git add Cargo.lock config
git -c user.name=pod-run -c user.email=pod-run@fabric.local commit -qm "chore: regenerate Cargo.lock and config projections"
git push origin "HEAD:refs/heads/$1"
git rev-parse HEAD

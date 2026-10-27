#!/usr/bin/env bash
# Linux build+test inside a rootless pod: system deps come from nixpkgs (no apt, no root).
set -euo pipefail
source "$(dirname "$0")/nix-env.sh"
set -x
# CROSS_TARGET=<triple>: only `cargo check` that target (no linking, no run).
if [ -n "${CROSS_TARGET:-}" ]; then
  rustup target add "$CROSS_TARGET"
  # Metal shaders cannot be compiled off macOS: compile them at runtime for the check (feature forwarded by oats-ui).
  exec cargo check --locked --workspace --target "$CROSS_TARGET" --features "oats-ui/runtime_shaders"
fi
cargo build --workspace --locked
cargo test --workspace --locked
cargo run -q --locked -- --headless --version
cargo build --locked -p oats-meetings-rs --no-default-features
cargo run -q --locked --no-default-features -- --headless --version

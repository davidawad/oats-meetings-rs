#!/usr/bin/env bash
# Linux build+test inside a rootless pod: system deps come from nixpkgs (no apt, no root).
set -euo pipefail
source "$(dirname "$0")/nix-env.sh"
[ -z "${SETUP_ONLY:-}" ] || return 0 2>/dev/null || exit 0
set -x
# main stays rustfmt-clean so pods' fmt steps are no-ops, not rebase conflicts (dot-4uzgol.42).
cargo fmt --all --check
# The rustc-wrapper every build goes through (.cargo/config.toml) passes through without a cache tool.
sh ci/rustc-wrapper-test.sh
# CROSS_TARGET=<triple>: only `cargo check` that target (no linking, no run).
if [ -n "${CROSS_TARGET:-}" ]; then
  rustup target add "$CROSS_TARGET"
  # build.rs embeds the derived .ico on Windows targets (dot-4uzgol.10).
  case "$CROSS_TARGET" in *windows*) env -u LD_LIBRARY_PATH bash ci/icons.sh ;; esac
  # Metal shaders cannot be compiled off macOS: compile them at runtime for the check (feature forwarded by oats-ui).
  # Windows has no local ASR yet (whisper-rs is not proven there): check it without the root's default `whisper`.
  nowhisper=()
  case "$CROSS_TARGET" in *windows*) nowhisper=(--no-default-features --features gui) ;; esac
  exec cargo check --locked --workspace --target "$CROSS_TARGET" "${nowhisper[@]}" --features "oats-ui/runtime_shaders"
fi
# Local ASR (whisper.cpp) is built and tested here; the model is fetched and checksummed, not committed.
source "$(dirname "$0")/fetch-models.sh"
# whisper (dot-4uzgol.26) and parakeet (dot-4uzgol.66) are default features of the root package; oats-core's are named here.
cargo build --workspace --locked --features oats-core/parakeet
cargo test --workspace --locked --features oats-core/parakeet
# One WER pass per allowed stt.local.model value, printed as WER_ROW lines (the recorded table).
set -o pipefail
cargo test -p oats-core --locked --features whisper,parakeet --test stt_whisper -- --nocapture --test-threads=1 2>&1 | grep "WER_ROW\|test result" | cut -c1-220
# The same engine tests without the local backend compiled in (backend_unavailable branch).
cargo test -p oats-core --locked --test stt_engine
# GUI-hosts-engine proof needs a virtual display. The pod has no GPU and nix's software Vulkan
# (lavapipe) cannot load against the pod's older host glibc, so no window opens here: the process
# must still serve the control socket and report window state honestly. Rendering is covered by
# the gpui test-support tests (crates/ui/src/view.rs).
if command -v Xvfb > /dev/null; then
  OATS_GUI_E2E=1 cargo test --locked --test gui_control -- --nocapture --test-threads=1
else
  echo "Xvfb not found: gui_control skipped"
fi
cargo run -q --locked -- --headless --version
cargo build --locked -p oats-meetings-rs --no-default-features
cargo run -q --locked --no-default-features -- --headless --version

#!/usr/bin/env bash
# STT benchmark (dot-4uzgol.22): every runtime x catalog model ci/fetch-models.sh fetches, WER / real-time factor / peak memory, live and final
# passes, clean + noise + crosstalk mixes of the LibriSpeech corpus. Pod only (cargo never runs on carbon):
#   bin/pod-run git@gitlab.com:davidawad/oats-meetings-rs.git <ref> bash ci/stt-bench.sh
# Prints BENCH_HOST, BENCH_ROW and BENCH_MEM lines; ci/stt-bench-table.sh turns a saved log into docs/stt-benchmark.md tables.
set -euo pipefail
source "$(dirname "$0")/nix-env.sh"
source "$(dirname "$0")/fetch-models.sh" >&2
source "$(dirname "$0")/fetch-bench-corpus.sh" >&2
echo "BENCH_HOST cpu=$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs) nproc=$(nproc) threads=4 commit=$(git rev-parse --short HEAD)"
run() { # features, engine, model
  OATS_BENCH_ENGINE=$2 OATS_BENCH_MODEL=$3 cargo test --locked --release -p oats-core --features "$1" --test stt_bench -- --nocapture --test-threads=1 2>&1 | grep -a -o 'BENCH_.*'
}
# OATS_BENCH_ONLY="base.en parakeet" runs a subset (default: everything).
only=" ${OATS_BENCH_ONLY:-tiny.en base.en small.en parakeet} "
for m in tiny.en base.en small.en; do [[ $only == *" $m "* ]] && run whisper whisper "$m"; done
[[ $only == *" parakeet "* ]] && run parakeet parakeet - || true

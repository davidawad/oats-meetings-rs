#!/usr/bin/env bash
# English STT benchmark (dot-4uzgol.67): fetch LibriSpeech test-clean/test-other (md5 checked
# against OpenSLR), set up a Python venv, then run bench.py for each model given.
#   bash bench/stt-en/run.sh parakeet-tdt-0.6b-v2 whisper-large-v3-turbo moonshine-base-en
# Env: BENCH_DATA (corpus dir, default /tmp/stt-bench-data), BENCH_MODELS_DIR, BENCH_THREADS.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
data=${BENCH_DATA:-/tmp/stt-bench-data}
venv=${BENCH_VENV:-$data/venv}
mkdir -p "$data"
cd "$data"
for split in test-clean test-other; do
  if [ ! -d "LibriSpeech/$split" ]; then
    curl -fsSLO "https://www.openslr.org/resources/12/$split.tar.gz"
    tar xzf "$split.tar.gz"
  fi
done
curl -fsSLO https://www.openslr.org/resources/12/md5sum.txt
grep -E 'test-(clean|other)\.tar\.gz' md5sum.txt | md5sum -c --ignore-missing
[ -x "$venv/bin/python" ] || uv venv -q "$venv" -p 3.12
VIRTUAL_ENV=$venv uv pip install -q sherpa-onnx==2.0.1 soundfile numpy faster-whisper huggingface_hub
for model in "$@"; do
  "$venv/bin/python" "$here/bench.py" run "$data/LibriSpeech" "$model"
done
"$venv/bin/python" "$here/bench.py" table

#!/usr/bin/env bash
# English STT benchmark (dot-4uzgol.67, .70): fetch LibriSpeech test-clean/test-other (md5 checked
# against OpenSLR) and the AMI SDM test parquet (pinned revision; segments sha256-checked against
# ami-subset.tsv), set up a Python venv, then run bench.py on both sets for each model given.
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
mkdir -p ami
for i in 0 1 2 3; do
  f=ami/test-0000$i-of-00004.parquet
  [ -f "$f" ] || curl -fsSL -o "$f" \
    "https://huggingface.co/datasets/edinburghcstr/ami/resolve/46f28f2503e2ec48f8867a84eef356c70476beab/sdm/test-0000$i-of-00004.parquet"
done
[ -x "$venv/bin/python" ] || uv venv -q "$venv" -p 3.12
VIRTUAL_ENV=$venv uv pip install -q sherpa-onnx==2.0.1 soundfile numpy faster-whisper huggingface_hub pyarrow
[ -d ami/segments ] || "$venv/bin/python" "$here/bench.py" ami-subset "$data/ami"
for model in "$@"; do
  "$venv/bin/python" "$here/bench.py" run "$data/LibriSpeech" "$model"
  "$venv/bin/python" "$here/bench.py" run-ami "$data/ami" "$model"
done
"$venv/bin/python" "$here/bench.py" table

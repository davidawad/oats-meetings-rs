#!/usr/bin/env bash
# Download the local-ASR test models and verify each SHA-256. Sourced by ci scripts; never committed.
# Fetches every value stt.local.model allows (keep in sync with STT_LOCAL_MODELS in
# crates/core/src/config/keys.rs; a test enforces it). Restrict with OATS_FETCH_MODELS="tiny.en base.en".
# Exports OATS_WHISPER_MODELS_DIR (holds ggml-<name>.bin) and OATS_WHISPER_MODEL (the tiny.en file).
# Cache dir: $OATS_MODELS_DIR.
set -euo pipefail
declare -A _sha=(
  [tiny.en]=921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f
  [tiny]=be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21
  [base.en]=a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002
  [base]=60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe
  [small.en]=c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d
  [small]=1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b
)
_dir=${OATS_MODELS_DIR:-${TMPDIR:-/tmp}/oats-models}
mkdir -p "$_dir"
for _m in ${OATS_FETCH_MODELS:-tiny.en tiny base.en base small.en small}; do
  _f=$_dir/ggml-$_m.bin
  if ! { [ -f "$_f" ] && echo "${_sha[$_m]}  $_f" | sha256sum -c --status -; }; then
    env -u LD_LIBRARY_PATH /usr/bin/curl -fL --retry 3 --retry-delay 5 -o "$_f.part" "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-$_m.bin"
    echo "${_sha[$_m]}  $_f.part" | sha256sum -c -
    mv "$_f.part" "$_f"
  fi
  echo "model: $_f ($(stat -c %s "$_f") bytes, sha256 ok)"
done
export OATS_WHISPER_MODELS_DIR=$_dir
export OATS_WHISPER_MODEL=$_dir/ggml-tiny.en.bin

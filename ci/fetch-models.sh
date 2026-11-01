#!/usr/bin/env bash
# Download the local-ASR test models and verify each SHA-256. Sourced by ci scripts; never committed.
# Fetches the small English whisper models of the model catalog crates/core/data/stt-models.json (each pin must equal the
# catalog's sha256; a test enforces it) and Parakeet v2. Restrict with OATS_FETCH_MODELS="tiny.en base.en".
# Exports OATS_WHISPER_MODELS_DIR (holds ggml-<name>.bin) and OATS_WHISPER_MODEL (the tiny.en file).
# Cache dir: $OATS_MODELS_DIR.
set -euo pipefail
declare -A _sha=(
  [tiny.en]=921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f
  [base.en]=a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002
  [small.en]=c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d
)
_dir=${OATS_MODELS_DIR:-${TMPDIR:-/tmp}/oats-models}
mkdir -p "$_dir"
for _m in ${OATS_FETCH_MODELS:-tiny.en base.en small.en}; do
  _f=$_dir/ggml-$_m.bin
  if ! { [ -f "$_f" ] && echo "${_sha[$_m]}  $_f" | sha256sum -c --status -; }; then
    env -u LD_LIBRARY_PATH /usr/bin/curl -fL --retry 3 --retry-delay 5 -o "$_f.part" "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-$_m.bin"
    echo "${_sha[$_m]}  $_f.part" | sha256sum -c -
    mv "$_f.part" "$_f"
  fi
  echo "model: $_f ($(stat -c %s "$_f") bytes, sha256 ok)"
done
# Parakeet TDT 0.6b v2 int8 (sherpa-onnx release asset), unpacked to $_dir/<PARAKEET_DIR>. OATS_FETCH_PARAKEET=0 skips.
if [ "${OATS_FETCH_PARAKEET:-1}" = 1 ]; then
  _pk=sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8
  _pk_sha=157c157bc51155e03e37d2466522a3a737dd9c72bb25f36eb18912964161e1ad
  if [ ! -f "$_dir/$_pk/.sha-ok-$_pk_sha" ]; then
    env -u LD_LIBRARY_PATH /usr/bin/curl -fL --retry 3 --retry-delay 5 -o "$_dir/$_pk.tar.bz2.part" "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/$_pk.tar.bz2"
    echo "$_pk_sha  $_dir/$_pk.tar.bz2.part" | sha256sum -c -
    tar -xjf "$_dir/$_pk.tar.bz2.part" -C "$_dir"
    : > "$_dir/$_pk.tar.bz2.part"
    touch "$_dir/$_pk/.sha-ok-$_pk_sha"
  fi
  echo "model: $_dir/$_pk (archive sha256 ok)"
fi
export OATS_WHISPER_MODELS_DIR=$_dir
export OATS_WHISPER_MODEL=$_dir/ggml-tiny.en.bin

#!/usr/bin/env bash
# Download the local-ASR test model and verify its SHA-256. Sourced by ci scripts; never committed.
# Exports OATS_WHISPER_MODEL (path to ggml-tiny.en.bin). Cache dir: $OATS_MODELS_DIR.
set -euo pipefail
_model=ggml-tiny.en.bin
_sha=921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f
_url=https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$_model
_dir=${OATS_MODELS_DIR:-${TMPDIR:-/tmp}/oats-models}
mkdir -p "$_dir"
_f=$_dir/$_model
if ! { [ -f "$_f" ] && echo "$_sha  $_f" | sha256sum -c --status -; }; then
  curl -fL --retry 3 --retry-delay 5 -o "$_f.part" "$_url"
  echo "$_sha  $_f.part" | sha256sum -c -
  mv "$_f.part" "$_f"
fi
export OATS_WHISPER_MODEL=$_f
echo "model: $_f ($(stat -c %s "$_f") bytes, sha256 ok)"

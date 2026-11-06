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
# Diarization DER fixture (crates/core/tests/diarize_ami.rs, dot-4uzgol.83): the AMI meetings of
# tests/fixtures/diarize-ami/manifest.tsv (url and sha256 columns) and the default sherpa-onnx speaker model
# (pinned to crates/core/data/speaker-models.json; a test enforces it). OATS_FETCH_DIARIZE=0 skips.
if [ "${OATS_FETCH_DIARIZE:-1}" = 1 ]; then
  _ami=$_dir/diarize-ami
  mkdir -p "$_ami/speakers"
  _spk=nemo_en_titanet_small.onnx
  _spk_sha=ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e
  _spk_url=https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/$_spk
  _rows=$(grep -v '^#' "$(dirname "${BASH_SOURCE[0]}")/../tests/fixtures/diarize-ami/manifest.tsv" | cut -f1-3 | sed 's|^\([^\t]*\)\t|\1.Array1-01.wav\t|')
  while IFS=$'\t' read -r _f _url _sha; do
    test -n "$_f" || continue
    if ! { [ -f "$_f" ] && echo "$_sha  $_f" | sha256sum -c --status -; }; then
      env -u LD_LIBRARY_PATH /usr/bin/curl -fL --retry 3 --retry-delay 5 -o "$_f.part" "$_url"
      echo "$_sha  $_f.part" | sha256sum -c -
      mv "$_f.part" "$_f"
    fi
    echo "diarize fixture: $_f (sha256 ok)"
  done <<< "$(printf '%s\n' "$_rows" | sed "s|^|$_ami/|"; printf '%s\t%s\t%s\n' "$_ami/speakers/$_spk" "$_spk_url" "$_spk_sha")"
  export OATS_DIARIZE_AMI_DIR=$_ami
  export OATS_DIARIZE_MODELS_DIR=$_ami/speakers
fi
export OATS_WHISPER_MODELS_DIR=$_dir
export OATS_WHISPER_MODEL=$_dir/ggml-tiny.en.bin

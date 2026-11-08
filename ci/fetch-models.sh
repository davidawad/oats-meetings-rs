#!/usr/bin/env bash
# Download the local-ASR test models and verify each SHA-256. Sourced by ci scripts; never committed.
# Every model file comes from the catalogs (crates/core/data/stt-models.json, speaker-models.json): its pinned
# revision url and its sha256, so there is no second URL or hash list here (dot-4uzgol.104; a test enforces it).
# Fetches the small English whisper models and Parakeet v2. Restrict with OATS_FETCH_MODELS="tiny.en base.en".
# Exports OATS_WHISPER_MODELS_DIR (holds ggml-<name>.bin) and OATS_WHISPER_MODEL (the tiny.en file).
# Cache dir: $OATS_MODELS_DIR.
set -euo pipefail
_data=$(dirname "${BASH_SOURCE[0]}")/../crates/core/data
_stt_ids=${OATS_FETCH_MODELS:-tiny.en base.en small.en}
# OATS_FETCH_PARAKEET=0 skips it.
_stt_ids_parakeet=parakeet-tdt-0.6b-v2
# The default sherpa-onnx speaker model, for tests/diarize_ami.rs.
_speaker_ids=titanet-small
_dir=${OATS_MODELS_DIR:-${TMPDIR:-/tmp}/oats-models}
mkdir -p "$_dir"
# _fetch_catalog <catalog.json> <id> <models dir>: every file of the entry into <models dir>/<entry dir>/<name>.
_fetch_catalog() {
  local _rows _f _url _sha _e
  _rows=$(env -u LD_LIBRARY_PATH jq -er --arg id "$2" \
    '.models[] | select(.id == $id) | .dir as $d | .files[] | [(if $d == "" then .name else $d + "/" + .name end), .url, .sha256] | @tsv' "$1") ||
    { echo "fetch-models: $2 is not in $1" >&2; return 1; }
  while IFS=$'\t' read -r _f _url _sha; do
    _f=$3/$_f
    mkdir -p "$(dirname "$_f")" || return 1
    if ! { [ -f "$_f" ] && echo "$_sha  $_f" | sha256sum -c --status -; }; then
      env -u LD_LIBRARY_PATH /usr/bin/curl -fL --retry 3 --retry-delay 5 -o "$_f.part" "$_url" &&
        echo "$_sha  $_f.part" | sha256sum -c - &&
        mv "$_f.part" "$_f" || return 1
    fi
  done <<< "$_rows"
  _e=$(env -u LD_LIBRARY_PATH jq -r --arg id "$2" '.models[] | select(.id == $id) | .dir' "$1")
  echo "model: $2 -> $3/$_e (sha256 ok)"
}
[ "${OATS_FETCH_PARAKEET:-1}" = 1 ] || _stt_ids_parakeet=
for _m in $_stt_ids $_stt_ids_parakeet; do
  _fetch_catalog "$_data/stt-models.json" "$_m" "$_dir" || return 1 2> /dev/null || exit 1
done
# Diarization DER fixture (crates/core/tests/diarize_ami.rs, dot-4uzgol.83): the AMI meetings of
# tests/fixtures/diarize-ami/manifest.tsv (url and sha256 columns) and the speaker model above. OATS_FETCH_DIARIZE=0 skips.
if [ "${OATS_FETCH_DIARIZE:-1}" = 1 ]; then
  _ami=$_dir/diarize-ami
  mkdir -p "$_ami/speakers"
  _rows=$(grep -v '^#' "$(dirname "${BASH_SOURCE[0]}")/../tests/fixtures/diarize-ami/manifest.tsv" | cut -f1-3 | sed 's|^\([^\t]*\)\t|\1.Array1-01.wav\t|')
  while IFS=$'\t' read -r _f _url _sha; do
    test -n "$_f" || continue
    _f=$_ami/$_f
    if ! { [ -f "$_f" ] && echo "$_sha  $_f" | sha256sum -c --status -; }; then
      env -u LD_LIBRARY_PATH /usr/bin/curl -fL --retry 3 --retry-delay 5 -o "$_f.part" "$_url"
      echo "$_sha  $_f.part" | sha256sum -c -
      mv "$_f.part" "$_f"
    fi
    echo "diarize fixture: $_f (sha256 ok)"
  done <<< "$_rows"
  for _m in $_speaker_ids; do
    _fetch_catalog "$_data/speaker-models.json" "$_m" "$_ami/speakers" || return 1 2> /dev/null || exit 1
  done
  export OATS_DIARIZE_AMI_DIR=$_ami
  export OATS_DIARIZE_MODELS_DIR=$_ami/speakers
fi
export OATS_WHISPER_MODELS_DIR=$_dir
export OATS_WHISPER_MODEL=$_dir/ggml-tiny.en.bin

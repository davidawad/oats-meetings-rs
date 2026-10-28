#!/usr/bin/env bash
# Fetch the STT benchmark corpus (dot-4uzgol.22) and verify every SHA-256. Sourced by ci/stt-bench.sh; audio is never committed.
# Source: LibriSpeech dev-clean + dev-other, CC BY 4.0 (https://www.openslr.org/12/). Per-clip licence, hash and reference
# text: tests/fixtures/stt-bench/manifest.tsv. Exports OATS_BENCH_DIR (holds <id>.wav, 16 kHz mono). Cache: $OATS_MODELS_DIR.
set -euo pipefail
declare -A _tar_sha=(
  [dev-clean]=76f87d090650617fca0cac8f88b9416e0ebf80350acb97b343a85fa903728ab3
  [dev-other]=12661c48e8c3fe1de2c1caa4c3e135193bfb1811584f11f569dd12645aa84365
)
_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
_dir=${OATS_MODELS_DIR:-${TMPDIR:-/tmp}/oats-models}/bench
mkdir -p "$_dir"
if ! command -v flac > /dev/null; then
  # Same pin as ci/nix-env.sh.
  _flac=$(nix --extra-experimental-features "nix-command flakes" build --no-link --print-out-paths "github:NixOS/nixpkgs/3ec636362e1816bc2f0888fa0fa2c69f1bc9e504#flac.bin" | head -1)
  export PATH="$_flac/bin:$PATH"
fi
for _s in dev-clean dev-other; do
  _t=$_dir/$_s.tar.gz
  if ! { [ -f "$_t" ] && echo "${_tar_sha[$_s]}  $_t" | sha256sum -c --status -; }; then
    env -u LD_LIBRARY_PATH /usr/bin/curl -fL --retry 3 --retry-delay 5 -o "$_t.part" "https://openslr.trmal.net/resources/12/$_s.tar.gz"
    echo "${_tar_sha[$_s]}  $_t.part" | sha256sum -c -
    mv "$_t.part" "$_t"
  fi
  # Only this set's clips from the manifest are extracted.
  _want=$(awk -F'\t' -v s="$_s" '!/^#/ && $2==s {split($5,a,"#"); print a[2]}' "$_root/tests/fixtures/stt-bench/manifest.tsv")
  # shellcheck disable=SC2086
  tar -xzf "$_t" -C "$_dir" $_want
done
while IFS=$'\t' read -r _id _s _lic _sex _url _sha _ref; do
  case $_id in '#'*|'') continue ;; esac
  _f=$_dir/${_url#*#}
  echo "$_sha  $_f" | sha256sum -c --status - || { echo "bench corpus: sha mismatch for $_id" >&2; exit 1; }
  flac -s -d -f -o "$_dir/$_id.wav" "$_f"
done < "$_root/tests/fixtures/stt-bench/manifest.tsv"
echo "bench corpus: $(find "$_dir" -maxdepth 1 -name '*.wav' | wc -l) clips in $_dir (sha256 ok, CC BY 4.0)"
export OATS_BENCH_DIR=$_dir

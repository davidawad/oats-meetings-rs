#!/usr/bin/env bash
# Fill tests/fixtures/private/ (gitignored) with David's 2026-11-06 meeting, both takes (dot-4uzgol.84).
# PRIVILEGED: carbon only. Never commit, push, upload or copy it anywhere else, never paste its text.
#   carbon:  bash ci/private-meeting.sh
#            cargo test -p oats-core --test private_meeting -- --ignored --nocapture --test-threads=1
# The tests print numbers only (PRIVATE_ROW lines). Optional hand-made inputs, also kept in the dir:
#   turns.tsv  hand-labelled remote turns: <start_s>\t<end_s>\t<speaker>; the diarization test seeds
#              turns.seed.tsv (the app's own labels) to correct and rename; drop its UNREVIEWED line.
#   terms.txt  one term per line for the term-hit counts (else long words repeated in the OpenOats take).
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
dst=$root/tests/fixtures/private
oats=${OATS_RS_SESSION:-$HOME/Library/Application Support/oats-meetings-rs/sessions/session_2026-11-06_16-01-11}
openoats=${OPENOATS_SESSION:-$HOME/Library/Application Support/OpenOats/sessions/session_2026-11-06_10-00-57}
notes=${OPENOATS_NOTES:-$HOME/Documents/oats-meetings/recordings/2026-11-06-1001-meeting.md}
cd "$root"
# Refuse to copy anywhere git would pick it up.
git check-ignore -q "$dst/audio.wav" || { echo "tests/fixtures/private/ is not gitignored: refusing" >&2; exit 1; }
for p in "$oats/audio.wav" "$oats/session.json" "$openoats/session.json" "$notes"; do
  [ -f "$p" ] || { echo "missing: $p" >&2; exit 1; }
done
mkdir -p "$dst/oats-rs" "$dst/openoats"
chmod 700 "$dst"
cp -Rp "$oats" "$dst/oats-rs/"
cp -Rp "$openoats" "$dst/openoats/"
cp -p "$notes" "$dst/openoats-notes.md"
hook=$root/.git/hooks/pre-commit
if [ -d "$root/.git" ] && [ -z "$(git config core.hooksPath || true)" ]; then
  if [ ! -e "$hook" ]; then
    ln -s ../../ci/hooks/pre-commit "$hook"
    echo "installed .git/hooks/pre-commit (refuses tracked private fixtures)"
  elif ! cmp -s "$hook" ci/hooks/pre-commit; then
    echo "note: .git/hooks/pre-commit exists; have it call ci/hooks/pre-commit" >&2
  fi
fi
[ -z "$(git ls-files -- tests/fixtures/private)" ] || { echo "tests/fixtures/private/ has tracked files" >&2; exit 1; }
echo "private fixture ready: $(find "$dst" -type f | wc -l | tr -d ' ') files"

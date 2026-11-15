#!/usr/bin/env bash
# release-notes.sh <version> [<from-ref>] [<to-ref>]: release notes for a version as markdown, from the
# conventional-commit subjects in from..to (default: last v* tag .. HEAD, or everything with no tag).
# Groups: Breaking changes, Features, Fixes, Other. Bead ids "(dot-...)" stay in the line. Merges and
# chore(release) commits are skipped.
set -euo pipefail
ver=${1:?usage: release-notes.sh <version> [from] [to]}
from=${2:-$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)}
to=${3:-HEAD}
range=${from:+$from..}$to
subjects=$(git log --no-merges --format='%s' "$range" | grep -v '^chore(release)' || true)
section() { # title, grep -E pattern, sed strip
  local lines
  lines=$(printf '%s\n' "$subjects" | grep -E "$2" | sed -E "$3" || true)
  [ -z "$lines" ] || { printf '\n### %s\n\n' "$1"; printf '%s\n' "$lines" | sed 's/^/- /'; }
}
printf '## %s (%s)\n' "$ver" "$(date +%Y-%m-%d)"
section "Breaking changes" '^[a-z]+(\([^)]*\))?!:' 's/^([a-z]+)(\(([^)]*)\))?!: /\3: /; s/^: //'
section "Features" '^feat(\([^)]*\))?:' 's/^feat(\(([^)]*)\))?: /\2: /; s/^: //'
section "Fixes" '^fix(\([^)]*\))?:' 's/^fix(\(([^)]*)\))?: /\2: /; s/^: //'
section "Other" '^(refactor|perf|docs|build|ci|test|chore|style|ui)(\([^)]*\))?:' 's/^[a-z]+(\(([^)]*)\))?: /\2: /; s/^: //'
printf '\n'

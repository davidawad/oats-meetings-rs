#!/usr/bin/env bash
# release.sh [--apply] [<version>|auto]: cut a release locally. Default is a dry run that prints the next
# version and its notes. --apply (clean tree, on main) bumps the workspace version in Cargo.toml and the
# oats-* entries in Cargo.lock, prepends the notes to CHANGELOG.md, commits "chore(release): vX" and tags vX
# (annotated). It never pushes, builds or publishes: that is a separate, deliberate step.
# auto: from the commits since the last v* tag. 0.x: a breaking change or a feat bumps minor, anything else
# patch. 1.x+: breaking major, feat minor, else patch.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
apply=0; [ "${1:-}" = --apply ] && { apply=1; shift; }
want=${1:-auto}
here=$(cd "$(dirname "$0")" && pwd)
cur=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
last=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)
subjects=$(git log --no-merges --format='%s%n%b' "${last:+$last..}HEAD" || true)
if [ "$want" = auto ]; then
  IFS=. read -r maj min pat <<< "$cur"
  breaking=0; feat=0
  # here-strings, not pipes: grep -q closing the pipe early would fail the pipeline under pipefail
  grep -qE '^[a-z]+(\([^)]*\))?!:|^BREAKING CHANGE' <<< "$subjects" && breaking=1
  grep -qE '^feat(\([^)]*\))?:' <<< "$subjects" && feat=1
  if [ "$maj" -ge 1 ]; then
    if [ $breaking = 1 ]; then maj=$((maj+1)); min=0; pat=0; elif [ $feat = 1 ]; then min=$((min+1)); pat=0; else pat=$((pat+1)); fi
  else
    if [ $breaking = 1 ] || [ $feat = 1 ]; then min=$((min+1)); pat=0; else pat=$((pat+1)); fi
  fi
  next="$maj.$min.$pat"
else
  next=${want#v}
fi
[[ $next =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "release: '$next' is not semver" >&2; exit 2; }
echo "release: $cur -> $next (since ${last:-the first commit})"
notes=$("$here/release-notes.sh" "$next" "$last")
printf '%s\n' "$notes"
[ $apply = 1 ] || { echo "release: dry run; pass --apply to commit and tag v$next (nothing is pushed)"; exit 0; }
[ -z "$(git status --porcelain)" ] || { echo "release: working tree is not clean" >&2; exit 1; }
[ "$(git rev-parse --abbrev-ref HEAD)" = main ] || { echo "release: not on main" >&2; exit 1; }
! git rev-parse -q --verify "refs/tags/v$next" > /dev/null || { echo "release: tag v$next exists" >&2; exit 1; }
sed -i.bak "0,/^version = \"$cur\"/s//version = \"$next\"/" Cargo.toml && rm -f Cargo.toml.bak
# Cargo.lock: the workspace members' own entries, so --locked builds keep working (cargo is not run here).
awk -v cur="$cur" -v nxt="$next" '
  /^name = "(oats-core|oats-ui|oats-ctl|oats-audio|oats-meetings-rs)"$/ { inws = 1; print; next }
  inws && $0 == "version = \"" cur "\"" { print "version = \"" nxt "\""; inws = 0; next }
  /^$/ { inws = 0 }
  { print }' Cargo.lock > Cargo.lock.new && mv Cargo.lock.new Cargo.lock
{ printf '# Changelog\n\n'; printf '%s\n' "$notes"; if [ -f CHANGELOG.md ]; then tail -n +2 CHANGELOG.md | sed '1{/^$/d}'; fi; } > CHANGELOG.md.new && mv CHANGELOG.md.new CHANGELOG.md
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -qm "chore(release): v$next"
git tag -a "v$next" -m "Oats Meetings v$next"
echo "release: committed and tagged v$next (not pushed)"

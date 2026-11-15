#!/usr/bin/env bash
# release-test.sh: ci/release.sh, release-notes.sh and make-cask.sh against a throwaway git repo.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
mkdir -p "$t/ci" "$t/packaging/macos"
cp "$here/release.sh" "$here/release-notes.sh" "$here/make-cask.sh" "$t/ci/"
cp "$here/../packaging/macos/oats-meetings.rb.in" "$t/packaging/macos/"
cd "$t"
git init -q -b main; git config user.email t@t; git config user.name t
printf '[workspace.package]\nversion = "0.0.1"\n' > Cargo.toml
printf '[[package]]\nname = "oats-core"\nversion = "0.0.1"\n\n[[package]]\nname = "serde"\nversion = "0.0.1"\n' > Cargo.lock
git add -A; git commit -qm "feat(core): first thing (dot-1)"
fail() { echo "FAIL: $*" >&2; exit 1; }
out=$(ci/release.sh auto); echo "$out" | grep -q "0.0.1 -> 0.1.0" || fail "first feat should bump 0.0.1 -> 0.1.0: $out"
grep -q "### Features" <<< "$out" || fail "notes: $out"
grep -q -- "- core: first thing (dot-1)" <<< "$out" || fail "notes: $out"
[ "$(git tag | wc -l)" = 0 ] || fail "dry run must not tag"
ci/release.sh --apply auto > /dev/null
git rev-parse -q --verify refs/tags/v0.1.0 > /dev/null || fail "tag v0.1.0 missing"
grep -q '^version = "0.1.0"' Cargo.toml || fail "Cargo.toml not bumped"
awk '/name = "oats-core"/{f=1;next} f&&/^version/{print;exit}' Cargo.lock | grep -q '"0.1.0"' || fail "oats-core lock entry not bumped"
awk '/name = "serde"/{f=1;next} f&&/^version/{print;exit}' Cargo.lock | grep -q '"0.0.1"' || fail "a third-party lock entry was changed"
grep -q "^## 0.1.0" CHANGELOG.md || fail "CHANGELOG missing 0.1.0"
for i in $(seq 1 400); do git commit -q --allow-empty -m "chore: filler $i"; done
git commit -q --allow-empty -m "fix(ui): a bug"
out=$(ci/release.sh auto); grep -q "0.1.0 -> 0.1.1" <<< "$out" || fail "fix should be a patch: $out"
git commit -q --allow-empty -m "feat(ui)!: break it"
out=$(ci/release.sh auto); grep -q "0.1.0 -> 0.2.0" <<< "$out" || fail "0.x breaking should bump minor: $out"
ci/release.sh --apply auto > /dev/null
grep -q "^## 0.2.0" CHANGELOG.md || fail "CHANGELOG missing 0.2.0"
grep -q "^## 0.1.0" CHANGELOG.md || fail "CHANGELOG should keep older entries"
echo dirty > x; ! ci/release.sh --apply 9.9.9 2> /dev/null || fail "dirty tree must refuse"; rm x
! ci/release.sh --apply 0.2.0 2> /dev/null || fail "an existing tag must refuse"
ci/release.sh not-a-version 2> /dev/null && fail "non-semver must refuse" || true
printf 'zipdata' > z.zip
cask=$(ci/make-cask.sh 0.2.0 z.zip https://example.com/dl https://example.com)
echo "$cask" | grep -q "sha256 \"$(shasum -a 256 z.zip | cut -d' ' -f1)\"" || fail "cask sha256"
echo "$cask" | grep -q 'url "https://example.com/dl/v#{version}/' || fail "cask url"
echo "release-test: ok"

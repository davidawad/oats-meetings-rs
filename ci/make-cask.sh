#!/usr/bin/env bash
# make-cask.sh <version> <zip> <base-url> <homepage>: print the Homebrew cask for a built release zip
# (Oats-Meetings-<version>-arm64.zip). The sha256 is computed from the zip. Nothing is uploaded.
set -euo pipefail
ver=${1:?usage: make-cask.sh <version> <zip> <base-url> <homepage>}
zip=${2:?zip}; base=${3:?base-url}; home=${4:?homepage}
[ -f "$zip" ] || { echo "make-cask: no such file $zip" >&2; exit 2; }
sha=$(shasum -a 256 "$zip" | cut -d' ' -f1)
sed -e "s|@VERSION@|$ver|g" -e "s|@SHA256@|$sha|g" -e "s|@BASE_URL@|${base%/}|g" -e "s|@HOMEPAGE@|$home|g" \
  "$(dirname "$0")/../packaging/macos/oats-meetings.rb.in"

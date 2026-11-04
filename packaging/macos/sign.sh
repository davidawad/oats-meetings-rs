#!/usr/bin/env bash
# Ad-hoc signing for Oats Meetings.app with an identifier-only designated requirement, so the
# Screen Recording and Microphone TCC grants survive rebuilds (dot-4uzgol.74). An ad-hoc
# signature's default DR is the cdhash, which changes on every build and orphans the grants.
# No Keychain, no security(1), no certificates: always `-s -`.
# Sourced by make-app.sh; tests/macos_signing.rs exercises it on Linux with a stub codesign.

# The bundle id from an Info.plist (plutil on macOS, the one-line key/string layout otherwise).
plist_bundle_id() {
  if command -v plutil >/dev/null 2>&1; then
    plutil -extract CFBundleIdentifier raw "$1"
  else
    sed -n 's|.*<key>CFBundleIdentifier</key><string>\([^<]*\)</string>.*|\1|p' "$1" | head -1
  fi
}

# The designated requirement text for a bundle id.
designated_requirement() {
  printf 'designated => identifier "%s"' "$1"
}

# Fails unless `codesign -dr -` output ($1) carries the identifier-only DR for bundle id $2.
check_designated_requirement() {
  case "$1" in
    *cdhash*) echo "sign: designated requirement still pins a cdhash: $1" >&2; return 1 ;;
  esac
  case "$1" in
    *"$(designated_requirement "$2")"*) ;;
    *) echo "sign: designated requirement lacks identifier \"$2\": $1" >&2; return 1 ;;
  esac
}

# Signs inside out: each bundled dylib, then the bundle (which seals the main binary) with the
# identifier-only DR; then checks the DR and runs a strict verify.
sign_app() {
  local app=$1 id lib dr
  id=$(plist_bundle_id "$app/Contents/Info.plist")
  [ -n "$id" ] || { echo "sign: no CFBundleIdentifier in $app/Contents/Info.plist" >&2; return 1; }
  for lib in "$app"/Contents/MacOS/*.dylib; do
    [ -e "$lib" ] || continue
    codesign --force -s - "$lib"
  done
  codesign --force -s - --identifier "$id" -r="$(designated_requirement "$id")" "$app"
  dr=$(codesign -dr - "$app" 2>&1)
  check_designated_requirement "$dr" "$id"
  # --deep here only verifies the nested dylibs; signing stays inside-out without it.
  codesign --verify --deep --strict "$app"
}

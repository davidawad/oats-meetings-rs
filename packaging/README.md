# Packaging

Icon: `assets/icon/AppIcon.png` (1024px) and `AppIcon.svg` are the blueprint "icon A" (David, 2026-10-30;
the upstream OpenOats icon is gone). Committed `assets/icon/hicolor/` holds sips-made Linux PNGs. `ci/icons.sh`
verifies the PNG sha256 and derives hicolor PNGs, the macOS iconset inputs and a Windows `.ico` into
`assets/icon/derived/` (gitignored).

    bin/pod-run git@gitlab.com:davidawad/oats-meetings-rs.git <ref> bash ci/icons.sh
    bin/pod-run git@gitlab.com:davidawad/oats-meetings-rs.git <ref> bash ci/package-linux.sh

## Linux
`ci/package-linux.sh` builds a release binary and produces a `.deb` (dpkg-deb) and an AppImage
(appimagetool) in `dist/`. The desktop file is `packaging/linux/oats-meetings-rs.desktop` with
`Icon=oats-meetings-rs`; hicolor icons install to `/usr/share/icons/hicolor/<N>x<N>/apps/`.

## macOS (proof waits for dot-4uzgol.15)
`packaging/macos/make-app.sh <binary> <outdir>` assembles `Oats Meetings.app` from
`Info.plist.in` (`CFBundleIconFile` = `AppIcon`), builds `AppIcon.icns` with `iconutil -c icns`,
and round-trips it with `iconutil -c iconset`, then ad-hoc signs it via `sign.sh`: dylibs first, then
the bundle with `-r='designated => identifier "<CFBundleIdentifier>"'` (no cdhash, so TCC grants
survive rebuilds), checks `codesign -dr -` and runs `codesign --verify --strict`. No Keychain or
certificates. Needs a Mac; `tests/macos_signing.rs` exercises `sign.sh` on Linux with a stub codesign.

## Windows
Not built here. A GPUI Windows binary needs the MSVC target and the Windows SDK, which does not
cross-build from a Linux pod (`CROSS_TARGET=x86_64-pc-windows-msvc` only type-checks). Plan: build
natively on a Windows host, embed `oats-meetings-rs.ico` as the exe resource (`winresource` build
script), and package an `.msi` with WiX (`wix build`) referencing the same `.ico` for the
Start Menu shortcut. The `.ico` is already derived and tested by `ci/icons.sh`.

## Signing (notes only; no keys in this repo)
- macOS: Developer ID Application cert, `codesign --options runtime --timestamp`, then
  `xcrun notarytool submit --wait` and `stapler staple`. Microphone use needs
  `NSMicrophoneUsageDescription` (present) and the audio-input entitlement.
- Windows: Authenticode via `signtool sign /fd sha256 /tr <timestamp-url> /td sha256` with an
  OV/EV cert, or Azure Trusted Signing.
- Linux: `.deb` repos sign the repo metadata (`gpg`/`reprepro`), AppImage supports an embedded
  `--sign` GPG signature. Keys come from Infisical at release time, never from the repo.

# CI

No hosted CI (GitHub or GitLab). CI = `bin/pod-run` running `ci/linux-nix.sh`.

    bin/pod-run git@gitlab.com:davidawad/oats-meetings-rs.git <ref> bash ci/linux-nix.sh
    bin/pod-run git@gitlab.com:davidawad/oats-meetings-rs.git <ref> env CROSS_TARGET=x86_64-pc-windows-msvc bash ci/linux-nix.sh

The first builds, tests and runs `--headless --version` on Linux (with and without the GUI feature). The second type-checks Windows. macOS is tracked in dot-4uzgol.15.

Local ASR: `whisper` is a default feature of the root package, so a stock build transcribes; the first recording downloads the configured `stt.local.model` (default base.en, `ctl stt model fetch` does it on demand). Parakeet stays opt-in (`--features oats-core/parakeet`). Windows builds pass `--no-default-features --features gui` (no local ASR there yet).

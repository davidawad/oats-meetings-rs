# CI

No hosted CI (GitHub or GitLab). CI = `bin/pod-run` running `ci/linux-nix.sh`.

    bin/pod-run git@gitlab.com:davidawad/oats-meetings-rs.git <ref> bash ci/linux-nix.sh
    bin/pod-run git@gitlab.com:davidawad/oats-meetings-rs.git <ref> env CROSS_TARGET=x86_64-pc-windows-msvc bash ci/linux-nix.sh

The first builds, tests and runs `--headless --version` on Linux (with and without the GUI feature). The second type-checks Windows. macOS is tracked in dot-4uzgol.15.

Local ASR: `whisper` (whisper.cpp) and `parakeet` (sherpa-onnx: Parakeet, Moonshine) are default features of the root package, so a stock build transcribes; app launch (or the first recording) downloads the configured `stt.local.model` from the model catalog `crates/core/data/stt-models.json` (default parakeet-tdt-0.6b-v2-coreml: Parakeet v2 on the Apple Neural Engine through CoreML on macOS, dot-4uzgol.69; elsewhere, or when CoreML cannot load it, parakeet-tdt-0.6b-v2 through sherpa-onnx on the CPU; `ctl stt model fetch` does it on demand). The CoreML runtime needs no extra libraries (CoreML is a system framework); its log-mel front end and TDT decode loop are Rust (`crates/core/src/stt/{mel,tdt}.rs`), tested on Linux against `crates/core/data/fixtures/parakeet-mel.json` (`ci/parakeet-mel-fixture.py`). sherpa-onnx links statically on Linux and as dylibs on macOS (the static macOS onnxruntime has no CoreML provider). Windows builds pass `--no-default-features --features gui` (no local ASR there yet).

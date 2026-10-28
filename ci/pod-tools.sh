#!/usr/bin/env bash
# What can a pod do? (throwaway probe)
uname -m; for t in python3 pip3 unzip curl wget sha256sum espeak-ng clang; do printf '%s: ' $t; command -v $t || echo none; done
python3 -c "import zipfile,sys;print('zipfile ok')" || true
curl -sI https://pypi.org/simple/libclang/ | head -1
curl -sI https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin | head -1

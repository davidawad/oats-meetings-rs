#!/usr/bin/env bash
clang --version | head -1
clang --print-resource-dir
ls -d /usr/lib/llvm*/lib/libclang* /usr/lib/x86_64-linux-gnu/libclang* /usr/lib64/libclang* 2>&1 | head
find / -name 'libclang*.so*' -not -path '*/nix/*' 2>/dev/null | head
which cmake g++ c++ make

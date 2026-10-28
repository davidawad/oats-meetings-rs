# Prebuilt gpui_windows shaders

gpui_windows (zed rev 37faef81f74fc1bb08be9939c4cffc8ede6ced2f) compiles its HLSL with fxc.exe in a build script
gated on the HOST os, so a Linux-hosted release cross-build has no shader bytes. `shaders_bytes.rs` is the same
output, produced inside a real Windows Server 2022 Evaluation VM by `tools/shadergen` (d3dcompiler_47.dll, flags
`/T vs_4_1|ps_4_1 /E <module>_{vertex,fragment} /O3`), via `TASK=shaders ci/windows-vm-run.sh`.
Pinned by `shaders_bytes.sha256` (equals the SHA256 certutil computed inside the VM) and by `sources.sha256` (the
HLSL inputs). `ci/package-windows.sh gui` refuses to build when either hash differs. When bumping the zed rev,
regenerate with the VM script and update both files.

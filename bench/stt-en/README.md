# English STT model benchmark for the catalog (dot-4uzgol.67)

Accuracy of the English candidates for `crates/core/data/stt-models.json` (dot-4uzgol.66), measured in
a pod on a fixed public set. Speed on Apple Silicon (ANE / MLX) can only be measured on a Mac, so it
is a PM live check (see the end).

## Corpus

- 300 LibriSpeech utterances: 200 from `test-clean`, 100 from `test-other`, picked by an even stride
  over the sorted utterance ids. `subset.tsv` lists each one with its flac sha256 and reference text
  (`bench.py` refuses a file whose hash differs). About 35 min of audio, 3993 + 1676 reference words.
- Licence: LibriSpeech is CC BY 4.0 (https://www.openslr.org/12/), so the reference text is committed.
  The audio is not: `run.sh` downloads `test-clean.tar.gz` / `test-other.tar.gz` and checks them
  against OpenSLR's `md5sum.txt`.
- AMI (meeting audio) was last in the order and is not done yet; see "Remaining".

## Method

- `bash bench/stt-en/run.sh <model>...` (or `bench.py run <LibriSpeech dir> <model>`), one model per
  process. A killed run resumes from `hyps/<model>.partial.tsv` (peak RSS is then the last pass's). Each utterance is decoded whole, greedy decoding, English forced where the model asks.
- WER: corpus-level per split (word edits / reference words). Both sides are lower-cased, plain
  integers spelled out, hyphens split, punctuation dropped, mr/mrs/dr expanded. Years written as digits
  (1999) still count against a model ("one thousand nine hundred ninety nine" vs "nineteen ninety nine").
- 300 utterances: differences under about 1 point (test-clean) or 2 points (test-other) are noise.
- Speed columns are pod CPU (shared AMD EPYC, 8-12 threads, `BENCH_THREADS`): RTF = decode wall time
  / audio time. Order of magnitude only; they are not laptop or ANE figures.
- Per-utterance hypotheses and edit counts: `hyps/<model>.tsv`; raw numbers: `results/<model>.json`.

## Results (measured)

| model | test-clean WER | test-other WER | weights | licence | RTF pod CPU | peak RSS MB | runtime measured |
|---|---|---|---|---|---|---|---|
| parakeet-tdt-0.6b-v2 | 2.25% | 4.53% | 661 MB (int8) | CC-BY-4.0 | 0.149 (8 thr) | 1253 | sherpa-onnx 2.0.1 int8, catalog files |
| moonshine-base-en | 3.81% | 10.92% | 287 MB (int8) | MIT | 0.331 (14 thr) | 558 (last resumed pass) | sherpa-onnx 2.0.1 int8, catalog files |

## Candidates: size, licence, Mac runtime, what Rust can drive

Sizes are the upstream checkpoint files on Hugging Face (checked 2026-11-01 via the HF API, the
sha is the repo revision). Rule from the bead thread: skip a model whose weights exceed 5 GB or need a
gated licence.

| candidate | upstream (revision) | licence | weights | status | Mac runtimes | Rust can drive |
|---|---|---|---|---|---|---|
| Parakeet TDT 0.6B v2 | nvidia/parakeet-tdt-0.6b-v2 (ae9ad07) | CC-BY-4.0 | 2.47 GB fp32; 661 MB int8 ONNX | measured | CoreML/ANE (FluidAudio, FluidInference/parakeet-tdt-0.6b-v2-coreml); MLX (mlx-community/parakeet-tdt-0.6b-v2); sherpa-onnx (CPU, CoreML EP) | yes: sherpa-onnx (in the app today); CoreML model via objc2 |
| Whisper large-v3-turbo | openai/whisper-large-v3-turbo (41f01f3) | MIT | 1.62 GB fp16; 574 MB ggml q5_0 | pending | whisper.cpp Metal (+ CoreML encoder); MLX (mlx-community/whisper-large-v3-turbo, mlx-audio) | yes: whisper.cpp via whisper-rs (in the app today) |
| Moonshine base (en) | moonshine-ai/moonshine-base (7a73d8d) | MIT | 0.25 GB | measured | sherpa-onnx (CPU) | yes: sherpa-onnx (in the app today) |
| Qwen3-ASR 0.6B | Qwen/Qwen3-ASR-0.6B (5eb1441) | Apache-2.0 | 1.88 GB | pending | MLX (qwen3-asr-mlx, mlx-audio, mlx-community/Qwen3-ASR-0.6B-8bit); speech-swift | MLX sidecar only |
| Kyutai STT 1B (en/fr) | kyutai/stt-1b-en_fr (1c34c6b) | CC-BY-4.0 | 2.36 GB | pending | MLX (moshi-mlx; aufklarer/Kyutai-STT-1B-MLX-8bit); candle Metal | yes: kyutai's own Rust/candle stack (moshi-server) or an MLX sidecar |
| Nemotron speech streaming en 0.6B | nvidia/nemotron-speech-streaming-en-0.6b (ebe59e5) | NVIDIA Open Model License (`license:other`) | 4.95 GB repo files | pending | MLX (animaslabs/nemotron-speech-streaming-en-0.6b-mlx) | MLX sidecar; sherpa-onnx only if an ONNX export is published |
| Granite Speech 4.0 1B | ibm-granite/granite-4.0-1b-speech (bd87ab8) | Apache-2.0 | 4.63 GB repo files | pending | MLX (mlx-audio, if supported) | MLX sidecar |
| Qwen3-ASR 1.7B | Qwen/Qwen3-ASR-1.7B (7278e1e) | Apache-2.0 | 4.70 GB | pending | MLX (qwen3-asr-mlx, mlx-audio); speech-swift | MLX sidecar only |
| Canary-Qwen 2.5B | nvidia/canary-qwen-2.5b (b1469e1) | CC-BY-4.0 | 5.12 GB | skipped: weights over 5 GB | none native (NeMo/PyTorch; community GGUF) | no |
| Cohere Transcribe 2B | CohereLabs/cohere-transcribe-03-2026 (d552dad) | Apache-2.0, but the HF repo is gated (`gated: auto`) | 4.13 GB | skipped: gated | CoreML (community 6-bit export) | objc2 CoreML, if the community export is trusted |

"pending": not measured in this run; the order of the remaining work is the bead thread's.

## Catalog entries

Only models with a runtime the app already has (sherpa-onnx, whisper.cpp) can be catalog entries
today. Their ids, URLs and sha256 are the ones already in `crates/core/data/stt-models.json`; this
benchmark adds the measured WER below (same files, same hashes):

| id | runtime | url / sha256 | WER test-clean / test-other (this set) |
|---|---|---|---|
| parakeet-tdt-0.6b-v2 | sherpa-onnx | catalog (csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8 @ 1ab9323) | 2.25% / 4.53% |
| moonshine-base-en | sherpa-onnx | catalog (sherpa-onnx moonshine-base-en int8 files) | 3.81% / 10.92% |

## PM live check (Mac)

Real-time factor, CPU % and peak memory on one 60 s clip per model with a Mac runtime (FluidAudio
CoreML/ANE for Parakeet, whisper.cpp Metal for Whisper turbo, MLX for Qwen3-ASR / Kyutai / Nemotron /
Granite). Read-only, no TCC prompts (file input, no microphone), downloaded models trashed afterwards.

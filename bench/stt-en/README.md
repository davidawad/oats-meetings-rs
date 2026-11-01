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
- AMI meeting subset (dot-4uzgol.70): 120 segments from the AMI SDM test split (single distant
  microphone, the AMI channel closest to a laptop on a meeting table), 3-30 s each, picked by an even
  stride over the sorted segment ids; all 16 test meetings appear. About 11.5 min of audio, 2027
  reference words. Source: the `edinburghcstr/ami` parquet at revision 46f28f2 (CC BY 4.0, Kaldi
  segmentation); `ami-subset.tsv` lists each segment with its wav sha256 and reference text, and
  `bench.py ami-subset` extracts the wavs from the downloaded parquet.

## Method

- `bash bench/stt-en/run.sh <model>...` (or `bench.py run <LibriSpeech dir> <model>`), one model per
  process. A killed run resumes from `hyps/<model>.partial.tsv` (peak RSS is then the last pass's). Each utterance is decoded whole, greedy decoding, English forced where the model asks.
- The PyTorch models run from their own venvs with the CPU torch 2.9 wheel: Qwen3-ASR with
  `qwen-asr` 0.0.6 (which pins transformers 4.57); Kyutai, Nemotron and Granite with
  `transformers==5.22.1` (plus `torchaudio`, `librosa`), whose native ports load
  `kyutai/stt-1b-en_fr-trfs`, the Nemotron safetensors and Granite directly (no `moshi` or NeMo
  install). Then `<venv>/bin/python bench.py run <LibriSpeech dir> <model>` and
  `bench.py run-ami <ami dir> <model>`; each loader names its packages.
- WER: corpus-level per split (word edits / reference words). Both sides are lower-cased, plain
  integers spelled out, hyphens split, punctuation dropped, mr/mrs/dr expanded. Years written as digits
  (1999) still count against a model ("one thousand nine hundred ninety nine" vs "nineteen ninety nine").
  AMI only: hesitations (um, uh, hmm, mm, mhm, er, erm, ah) are also dropped from both sides, since the
  AMI references mark them and most models omit them. AMI references keep partial words and
  repetitions, so AMI WER is higher than published AMI figures that use a full text normaliser.
- 300 utterances: differences under about 1 point (test-clean) or 2 points (test-other) are noise;
  on the 120 AMI segments, under about 3 points.
- Speed columns are pod CPU (shared AMD EPYC, `BENCH_THREADS`): RTF = decode wall time / audio time.
  The pod's cgroup allows 4 CPUs (`cpu.max` 400000/100000), so runs with more threads than 4, or two
  models at once, were throttled and their RTF is pessimistic. Order of magnitude only; they are not
  laptop or ANE figures.
- Per-utterance hypotheses and edit counts: `hyps/<model>.tsv`; raw numbers: `results/<model>.json`.

## Results (measured)

| model | test-clean WER | test-other WER | weights | licence | RTF pod CPU | peak RSS MB | runtime measured |
|---|---|---|---|---|---|---|---|
| parakeet-tdt-0.6b-v2 | 2.25% | 4.53% | 661 MB (int8) | CC-BY-4.0 | 0.149 (8 thr) | 1253 | sherpa-onnx 2.0.1 int8, catalog files |
| moonshine-base-en | 3.81% | 10.92% | 287 MB (int8) | MIT | 0.331 (14 thr) | 558 (last resumed pass) | sherpa-onnx 2.0.1 int8, catalog files |
| whisper-large-v3-turbo | 2.40% | 5.31% | 1.62 GB fp16 CT2 (int8 at load) | MIT | 1.04 (12, 8 then 4 thr over 5 resumed passes, partly throttled) | 1659 (last resumed pass) | faster-whisper (CTranslate2 int8), greedy, English forced |
| qwen3-asr-0.6b | 2.40% | 6.98% | 1.88 GB (bf16 checkpoint) | Apache-2.0 | 0.67 (4 thr) | 6017 (fp32, last resumed pass) | qwen-asr 0.0.6 (torch 2.9 CPU, transformers 4.57) backend, fp32 CPU, language forced to English |
| qwen3-asr-1.7b | 1.83% | 5.31% | 4.70 GB (bf16 checkpoint) | Apache-2.0 | 1.69 (4 thr) | 11787 (fp32) | qwen-asr 0.0.6 (torch 2.9 CPU, transformers 4.57) backend, fp32 CPU, language forced to English |
| nemotron-speech-streaming-en-0.6b | 2.55% | 6.26% | 2.47 GB (safetensors) | NVIDIA Open Model License | 0.357 (1 thr) | 3274 (fp32) | transformers 5.22.1 (torch 2.9 CPU) port, fp32, whole utterance, 1.12 s chunk attention, greedy RNN-T |
| granite-speech-4.0-1b | 1.75% | 4.00% | 4.64 GB (repo files, bf16) | Apache-2.0 | 1.57 (1 thr; two 1-thread workers, one decoding from the end of the list) | not measured | transformers 5.22.1 (torch 2.9 CPU), bf16, model-card transcription prompt, greedy |

Nemotron and Granite (dot-4uzgol.72): Granite has the lowest WER on both splits (within noise of
Qwen3-ASR 1.7B on test-clean); Nemotron is within noise of Parakeet. Neither has a runtime the app
ships.

### AMI SDM meeting subset (dot-4uzgol.72)

The catalog models, same runtimes and files as above, on the 120 AMI SDM segments (1959 reference
words after dropping hesitations). This is the meeting-audio figure for the app default.

| model | AMI SDM WER | hypothesis words | RTF pod CPU | peak RSS MB |
|---|---|---|---|---|
| parakeet-tdt-0.6b-v2 | 20.98% | 1879 | 0.082 (4 thr) | 1083 |
| whisper-large-v3-turbo | 27.67% | 1770 | 1.47 (4 thr, shared the 4 CPUs with a 2-thread Kyutai run for most of it) | 1659 |
| moonshine-base-en | 31.90% | 1949 | 0.058 (4 thr) | 496 |
| nemotron-speech-streaming-en-0.6b (not a catalog model) | 21.18% | 1783 | 0.277 (1 thr) | 3025 |

Parakeet leads by about 7 points, well past the 3-point noise on this set. Whisper turbo's gap is
mostly deletions: it emits 1770 words against 1959 reference words, dropping quieter far-field
speech (for example "or something like that." for a 15-word segment). Moonshine keeps the word count
(1949) but has the most errors. Nemotron streaming (no app runtime yet) ties Parakeet within noise.

## Candidates: size, licence, Mac runtime, what Rust can drive

Sizes are the upstream checkpoint files on Hugging Face (checked 2026-11-01 via the HF API, the
sha is the repo revision). Rule from the bead thread: skip a model whose weights exceed 5 GB or need a
gated licence.

| candidate | upstream (revision) | licence | weights | status | Mac runtimes | Rust can drive |
|---|---|---|---|---|---|---|
| Parakeet TDT 0.6B v2 | nvidia/parakeet-tdt-0.6b-v2 (ae9ad07) | CC-BY-4.0 | 2.47 GB fp32; 661 MB int8 ONNX | measured | CoreML/ANE (FluidAudio, FluidInference/parakeet-tdt-0.6b-v2-coreml); MLX (mlx-community/parakeet-tdt-0.6b-v2); sherpa-onnx (CPU, CoreML EP) | yes: sherpa-onnx (in the app today); CoreML model via objc2 |
| Whisper large-v3-turbo | openai/whisper-large-v3-turbo (41f01f3) | MIT | 1.62 GB fp16; 574 MB ggml q5_0 | measured | whisper.cpp Metal (+ CoreML encoder); MLX (mlx-community/whisper-large-v3-turbo, mlx-audio) | yes: whisper.cpp via whisper-rs (in the app today) |
| Moonshine base (en) | moonshine-ai/moonshine-base (7a73d8d) | MIT | 0.25 GB | measured | sherpa-onnx (CPU) | yes: sherpa-onnx (in the app today) |
| Qwen3-ASR 0.6B | Qwen/Qwen3-ASR-0.6B (5eb1441) | Apache-2.0 | 1.88 GB | measured | MLX (qwen3-asr-mlx, mlx-audio, mlx-community/Qwen3-ASR-0.6B-8bit); speech-swift | MLX sidecar only |
| Kyutai STT 1B (en/fr) | kyutai/stt-1b-en_fr (1c34c6b) | CC-BY-4.0 | 2.36 GB | skipped: pod RTF 3.13 (above 3) | MLX (moshi-mlx; aufklarer/Kyutai-STT-1B-MLX-8bit); candle Metal | yes: kyutai's own Rust/candle stack (moshi-server) or an MLX sidecar |
| Nemotron speech streaming en 0.6B | nvidia/nemotron-speech-streaming-en-0.6b (ebe59e5) | NVIDIA Open Model License (`license:other`) | 4.95 GB repo files (2.47 GB safetensors loaded) | measured | MLX (animaslabs/nemotron-speech-streaming-en-0.6b-mlx) | MLX sidecar; sherpa-onnx only if an ONNX export is published |
| Granite Speech 4.0 1B | ibm-granite/granite-4.0-1b-speech (bd87ab8) | Apache-2.0 | 4.63 GB repo files | measured | MLX (mlx-audio, if supported) | MLX sidecar |
| Qwen3-ASR 1.7B | Qwen/Qwen3-ASR-1.7B (7278e1e) | Apache-2.0 | 4.70 GB | measured | MLX (qwen3-asr-mlx, mlx-audio); speech-swift | MLX sidecar only |
| Canary-Qwen 2.5B | nvidia/canary-qwen-2.5b (b1469e1) | CC-BY-4.0 | 5.12 GB | skipped: weights over 5 GB | none native (NeMo/PyTorch; community GGUF) | no |
| Cohere Transcribe 2B | CohereLabs/cohere-transcribe-03-2026 (d552dad) | Apache-2.0, but the HF repo is gated (`gated: auto`) | 4.13 GB | skipped: gated | CoreML (community 6-bit export) | objc2 CoreML, if the community export is trusted |

## Catalog entries

Only models with a runtime the app already has (sherpa-onnx, whisper.cpp) can be catalog entries
today. Their ids, URLs and sha256 are the ones already in `crates/core/data/stt-models.json`; this
benchmark adds the measured WER below (same files, same hashes):

| id | runtime | url / sha256 | WER test-clean / test-other (this set) | AMI SDM WER |
|---|---|---|---|---|
| parakeet-tdt-0.6b-v2 | sherpa-onnx | catalog (csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8 @ 1ab9323) | 2.25% / 4.53% | 20.98% |
| moonshine-base-en | sherpa-onnx | catalog (sherpa-onnx moonshine-base-en int8 files) | 3.81% / 10.92% | 31.90% |
| whisper-large-v3-turbo | whisper.cpp | catalog (ggml large-v3-turbo q5_0) | 2.40% / 5.31% (measured with CTranslate2 int8, same weights; q5_0 may differ slightly) | 27.67% |

## Remaining

- Kyutai STT 1B: skipped (dot-4uzgol.72 rule: skip a model whose pod RTF is above 3). Measured
  pod RTF 3.13 (bf16, 2 threads, sharing the 4 CPUs with other runs, so a clean 4-thread run would
  be somewhat faster) over the first 136 test-clean utterances (1004 s of audio); WER on those 136
  was 4.81% (2746 reference words), against 1.75-3.81% for the other models on the full test-clean
  200. Part of that is the normaliser dropping accented letters from Kyutai's French spellings
  ("pègre" scores as "p gre"). Hypotheses so far: `hyps/kyutai-stt-1b-en_fr.partial.tsv` (a rerun
  resumes from it).
- Nemotron, Kyutai and Granite have no runtime the app ships, so none can be a catalog entry yet.
- AMI meeting subset for Qwen3-ASR, Kyutai and Granite: `run-ami` in place; the catalog models and
  Nemotron are measured above.

## PM live check (Mac)

Real-time factor, CPU % and peak memory on one 60 s clip per model with a Mac runtime (FluidAudio
CoreML/ANE for Parakeet, whisper.cpp Metal for Whisper turbo, MLX for Qwen3-ASR / Kyutai / Nemotron /
Granite). Read-only, no TCC prompts (file input, no microphone), downloaded models trashed afterwards.

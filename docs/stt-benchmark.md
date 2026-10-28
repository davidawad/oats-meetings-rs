# STT benchmark: engine x model (dot-4uzgol.22)

Measurements for David's final review. No default was changed by this work.

## Corpus and licence

- 16 clips (about 189 s, 501 reference words): 8 from LibriSpeech `dev-clean`, 8 from `dev-other` (the harder, noisier-read half), 16 different speakers, 10 male and 6 female.
- Licence: LibriSpeech is CC BY 4.0 (Panayotov, Chen, Povey, Khudanpur 2015). Checked 2026-10-28 at https://www.openslr.org/12/ and in the archive's `LibriSpeech/LICENSE.TXT` ("licensed under a Creative Commons Attribution 4.0 International License", https://creativecommons.org/licenses/by/4.0/). CC BY 4.0 allows private use and redistribution with attribution, so the per-clip text, hashes and licence are in `tests/fixtures/stt-bench/manifest.tsv` (column `licence`; the test refuses a row that is not `CC-BY-4.0`).
- Audio is not committed. `ci/fetch-bench-corpus.sh` downloads `dev-clean.tar.gz` and `dev-other.tar.gz` (sha256 pinned, the pins agree with OpenSLR's `md5sum.txt`), verifies every extracted flac against its pinned sha256, and decodes to 16 kHz mono wav.
- AMI Meeting Corpus (CC BY 4.0) was considered and not used: its audio is hours-long multi-channel meetings that need per-segment alignment from separate annotation zips, too heavy for a small pinned corpus. It would be the right next step for real crosstalk.
- Mixes, generated deterministically at test time (derived works inherit CC BY 4.0 from their sources):
  - `clean`: the clip.
  - `noise10db`: seeded smoothed (pink-ish) noise at 10 dB SNR.
  - `crosstalk5db`: another corpus clip (clip i+5) starting 0.5 s in, as an overlapping talker at 5 dB SNR (target speaker louder).
- Not covered: accents. LibriSpeech carries no accent labels, so `dev-other` stands in for hard speakers; this does not test non-native or regional accents.

## Method

- `ci/stt-bench.sh` in a pod, one process per engine x model (so peak memory is that config's own). 4 threads (the test default `stt.local.threads`), release build, `cargo test --locked --release -p oats-core --test stt_bench` with `OATS_BENCH_ENGINE` and `OATS_BENCH_MODEL`.
- Live pass: `stt::Live` with the default `stt.live.chunk_secs = 4`, fed 0.5 s at a time as `stt::run` does. Final pass: the whole clip in one `transcribe` call (the final pass's own behaviour for audio under 10 minutes).
- WER: corpus-level (total word edits over total reference words) using `stt::wer`, which lower-cases and ignores punctuation. Whisper writes digits for spelled numbers, which counts against it a little.
- RTF = wall time / audio seconds for the pass (lower is faster; above 1.0 is slower than real time on 4 threads). Peak memory = `VmHWM` of the process after both passes and all conditions, including the model. Model load time is separate.
- Host: AMD EPYC-Genoa pod, 16 visible cores (shared cluster pool), `BENCH_HOST` in the logs. Commit `f886bbf` (6 whisper models, parakeet crosstalk/noise/final rows) and `92b4cb2` (parakeet re-run, base.en repeat): the second run only fixed a log-parsing bug that dropped three parakeet rows.
- The first run's parakeet RTF and the repeated base.en show how noisy RTF is on a shared pod: base.en live RTF 0.661 then 0.279, parakeet final 0.065 then 0.042. Treat RTF as order-of-magnitude; WER and memory are stable.

## Results

WER over all 16 clips, per pass:

| engine/model | clean live | clean final | noise live | noise final | crosstalk live | crosstalk final |
|---|---|---|---|---|---|---|
| whisper/tiny.en | 12.4% | 7.0% | 26.4% | 19.4% | 73.9% | 56.1% |
| whisper/tiny | 15.8% | 10.2% | 35.5% | 20.6% | 72.3% | 58.5% |
| whisper/base.en | 10.6% | 4.4% | 21.4% | 10.4% | 49.3% | 34.3% |
| whisper/base | 12.0% | 5.8% | 27.5% | 14.6% | 58.9% | 40.5% |
| whisper/small.en | 7.4% | 3.0% | 12.4% | 4.4% | 36.3% | 19.0% |
| whisper/small | 9.6% | 3.4% | 18.8% | 8.0% | 35.5% | 22.4% |
| parakeet/tdt-0.6b-v2-int8 | 10.2% | 0.8% | 10.6% | 1.2% | 47.5% | 31.9% |

| engine/model | clean final, dev-clean | clean final, dev-other | RTF live (clean) | RTF final (clean) | peak RSS MB | model load s |
|---|---|---|---|---|---|---|
| whisper/tiny.en | 0.8% | 13.0% | 0.196 | 0.060 | 305 | 0.11 |
| whisper/tiny | 4.8% | 15.4% | 0.198 | 0.064 | 279 | 0.10 |
| whisper/base.en | 0.4% | 8.3% | 0.661 | 0.164 | 409 | 0.16 |
| whisper/base | 2.4% | 9.1% | 0.390 | 0.124 | 389 | 0.25 |
| whisper/small.en | 0.4% | 5.5% | 1.451 | 0.450 | 845 | 0.60 |
| whisper/small | 1.6% | 5.1% | 1.468 | 0.575 | 828 | 0.66 |
| parakeet/tdt-0.6b-v2-int8 | 0.0% | 1.6% | 0.059 | 0.042 | 1620 | 3.50 |

(second table: WER of the clean final pass split by subset; RTF from run 1, parakeet and base.en live RTFs as noted above; parakeet RTF from run 2)

## Reading it

- Final pass, clean: parakeet 0.8%, small.en 3.0%, base.en 4.4%, tiny.en 7.0%. Parakeet is also faster than every whisper model above tiny and the best on noise (1.2% final at 10 dB SNR), at 1.6 GB peak RSS and a 3.5-4 s load.
- English-only (`.en`) models beat their multilingual twins at every size on this English corpus.
- Live pass is roughly 2-3x worse than final for every model (4 s windows split phrases). The sole exception is parakeet under noise, where live and final both stay lower than any whisper model.
- Crosstalk at 5 dB SNR is hard for all of them (parakeet 32%, small.en 19%, base.en 34% final): none separates the overlapping talker, they transcribe a mix. That needs diarization / channel separation, not a model default.
- small and small.en run slower than real time in the live pass on 4 threads (RTF above 1); on a slower laptop, or with a second engine running, live small would fall behind.
- Sample size is small (501 words); differences of about 1-2 points are not significant.

Reproduce: `bin/pod-run git@gitlab.com:davidawad/oats-meetings-rs.git <ref> bash ci/stt-bench.sh > log`, then `bash ci/stt-bench-table.sh log`.

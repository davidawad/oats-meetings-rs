# STT benchmark: engine x model (dot-4uzgol.22)

Measurements for David's final review. No default was changed by this work.

## Corpus and licence

- 16 clips (about 189 s, 501 reference words): 8 from LibriSpeech `dev-clean`, 8 from `dev-other` (the harder, noisier-read half), 16 different speakers, 11 male and 5 female.
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
- Each clip goes through the real pipeline `stt::run` with the default config, mic channel only. Keys at their defaults: `stt.speech_peak=10` (permille), `stt.pad=250ms`, `stt.live.chunk=4s`, `stt.feed=500ms`, `stt.final_piece=10m` (the whole clip, one piece).
- WER: corpus-level (total word edits over total reference words) using `stt::wer`, which lower-cases and ignores punctuation. Whisper writes digits for spelled numbers, which counts against it a little.
- RTF = wall time / audio seconds for the pass (lower is faster; above 1.0 is slower than real time on 4 threads). Peak memory = `VmHWM` of the process after both passes and all conditions, including the model. Model load time is separate.
- Host: AMD EPYC-Genoa pod, 16 visible cores on a shared cluster pool (`BENCH_HOST` in the log). One full run, commit `ed805cf`, pod job `run-1028035602-39077`. Earlier runs of a first harness version (own windowing, jobs `run-1028010052-65318`, `run-1028030815-88782`) agreed with this one on WER within about 2 points and varied by up to 2.4x in RTF on the same model (base.en live 0.661 vs 0.279), so treat RTF as order-of-magnitude on a shared pod; WER and memory are stable.

## Results

WER over all 16 clips, per pass (first table); clean final-pass WER by subset, real-time factor on clean audio, peak memory and model load time (second table):

| engine/model | clean live | clean final | noise live | noise final | crosstalk live | crosstalk final |
|---|---|---|---|---|---|---|
| whisper/tiny.en | 12.4% | 6.8% | 26.4% | 19.4% | 73.9% | 56.5% |
| whisper/tiny | 15.8% | 10.4% | 35.5% | 20.6% | 72.3% | 60.1% |
| whisper/base.en | 10.6% | 5.2% | 21.4% | 10.4% | 49.3% | 42.1% |
| whisper/base | 12.0% | 6.0% | 27.5% | 14.6% | 58.9% | 38.7% |
| whisper/small.en | 7.4% | 2.0% | 12.4% | 4.4% | 36.3% | 27.5% |
| whisper/small | 9.6% | 3.6% | 18.8% | 8.0% | 35.5% | 20.6% |
| parakeet/tdt-0.6b-v2-int8 | 10.2% | 1.2% | 10.6% | 1.2% | 47.5% | 31.3% |

| engine/model | clean final, dev-clean | clean final, dev-other | RTF live (clean) | RTF final (clean) | peak RSS MB | model load s |
|---|---|---|---|---|---|---|
| whisper/tiny.en | 0.8% | 12.7% | 0.203 | 0.071 | 280 | 0.14 |
| whisper/tiny | 5.7% | 15.0% | 0.208 | 0.065 | 308 | 0.13 |
| whisper/base.en | 1.2% | 9.1% | 0.484 | 0.153 | 392 | 0.17 |
| whisper/base | 3.2% | 8.7% | 0.404 | 0.129 | 396 | 0.17 |
| whisper/small.en | 0.4% | 3.6% | 1.332 | 0.410 | 849 | 0.46 |
| whisper/small | 1.6% | 5.5% | 1.334 | 0.423 | 835 | 0.57 |
| parakeet/tdt-0.6b-v2-int8 | 0.0% | 2.4% | 0.101 | 0.066 | 1623 | 4.01 |

## Reading it

- Final pass, clean: parakeet 1.2%, small.en 2.0%, small 3.6%, base.en 5.2%, base 6.0%, tiny.en 6.8%, tiny 10.4%. Under 10 dB noise parakeet stays at 1.2% final and 10.6% live; the best whisper (small.en) is 4.4% final and 12.4% live. Parakeet's RTF (0.07 final, 0.10 live) is close to tiny's, at 1.6 GB peak RSS and a 4 s load; small.en is 849 MB.
- English-only (`.en`) models beat their multilingual twins at every size on clean and noisy final passes here, except base/base.en on dev-other and the crosstalk rows (small differences, see sample-size note).
- Live pass is 2-6x worse than final on clean audio for every model except parakeet-under-noise (4 s windows split phrases).
- small and small.en run slower than real time in the live pass on 4 threads (RTF 1.33); on a slower laptop, or with a second engine running, live small would fall behind. Parakeet and every model up to base.en keep up.
- Crosstalk at 5 dB SNR is hard for all of them (final: small 20.6%, parakeet 31.3%, base 38.7%): none separates the overlapping talker, they transcribe a mix. That needs diarization / channel separation, not a model default.
- Sample size is small (501 words): differences of about 1-2 points between neighbours are not significant.

Reproduce: `bin/pod-run git@gitlab.com:davidawad/oats-meetings-rs.git <ref> bash ci/stt-bench.sh > log`, then `bash ci/stt-bench-table.sh log`.

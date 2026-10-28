Synthetic speech fixtures for the transcription engine (dot-4uzgol.8). No real person's voice.

Source: macOS text-to-speech, generated on 2026-10-28 on carbon:
  say -v Karen  -o you.aiff  "<text in expected.json, you.wav>"
  say -v Daniel -o them.aiff "<text in expected.json, them.wav>"
  afconvert -f WAVE -d LEI16@16000 -c 1 <name>.aiff <name>.wav      (16 kHz mono 16-bit PCM)

you.wav  = the microphone channel ("you");  them.wav = the system-audio channel ("them").
expected.json holds the reference transcripts the word error rate is measured against.
Acceptance: WER <= 0.20 per channel with the ggml-tiny.en model (see ci/fetch-models.sh).

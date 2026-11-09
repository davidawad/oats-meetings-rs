# Auto-stop and auto-start

A recording can stop by itself when a meeting is over, and start by itself when one begins
(dot-4uzgol.110). Both listen to the levels of the two channels, system audio (what the speakers
play) and the microphone, in 10 ms frames: a frame is loud when its peak reaches a level given in
per-mille of full scale (10 = 0.01), the gate `stt.speech_peak` uses.

## Auto-stop (on by default)

While recording, once **both** channels stay below `record.auto_stop.silence_level` for
`record.auto_stop.after` (default `5m`), the recording stops:

- the silent stretch after the last sound is cut from the saved `audio.wav` (keeping
  `record.auto_stop.tail`, default `2s`, so the last word is not clipped), and so are the live
  utterances in it; the final transcript is made from the cut audio. `session.json` records the cut
  as `autoStop: {cutAtMs, trimmedMs}`;
- the window shows "Recording stopped after 5m of silence", `recording-stopped` has
  `by: "auto-stop"` and `trimmed_ms`, and the `auto-stopped` event is published.

`record.auto_stop.warn_before` (default `30s`, `0s` for none) warns first: an `auto-stop-pending`
event and a window banner with **Keep recording**, which runs `record-keep`. Keeping, or sound
coming back, publishes `auto-stop-cancelled` (`by` is who kept it, or `sound`) and the silence
starts over.

Silence is audio time: a paused recording does not count, and a resume starts it over. Sound
shorter than `record.auto_stop.blip` (default `300ms`; a notification ping, a click) does not end
the silence.

## Auto-start (off by default)

With `record.auto_start.enabled`, while idle, once `record.auto_start.sources` (`speaker`, `mic` or
`either`; default `speaker`) stays at or above `record.auto_start.level` for
`record.auto_start.after` (default `30s`), a recording starts: `recording-started` carries
`auto_start: {source, after}`, the `auto-started` event is published and the window says
"Recording started: speaker audio for 30s".

Guards against false starts:

- the sound must be sustained: a dip shorter than `record.auto_start.gap` (default `2s`) is part
  of it, a longer one starts the count over, so notification sounds and short clips start nothing;
- `record.auto_start.cooldown` (default `2m`): nothing starts this soon after a recording stopped;
- `record.auto_start.only_in_event`: only while a calendar event (`calendar.sources`) is on.

Auto-start listens through the input monitor, so enabling it keeps the monitor open while the app
runs (as `ui.monitor = always` does; on macOS the mic indicator stays on).

## Control

Every key above is a config key (`config/defaults.toml`, `[record]`), editable in Settings >
Recording and over ctl:

```sh
ctl config-set record.auto_stop.after 10m
ctl config-set record.auto_start.enabled true
ctl record-keep                 # cancel a pending auto-stop
ctl record-auto-status          # silence so far, pending stop, idle activity per source
ctl subscribe topics='["auto-stop-pending","auto-stop-cancelled","auto-stopped","auto-started"]'
```

`record.auto_check` (default `250ms`) is how often the engine looks. For a desktop notification,
add a hook on `record.auto_stop` or `record.auto_start` (e.g. running `notify-send`); the app has no
notification service of its own.

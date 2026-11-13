# Hooks v2: an event contract an agent can drive

Epic: dot-4uzgol.111. Integration branch: `feat/hooks-v2` (every child bead
lands there; the quality gate runs once, on the branch, before it merges to
`main`).

## What exists

`[[hooks]]` tables (`crates/core/src/hooks.rs`): `event`, `command` (argv, or a
script with `shell = true`), `timeout`, `enabled`, `cwd`. Events:
`record.start`, `record.pause`, `record.stop`, `record.auto_start`,
`record.auto_stop`, `transcript.final`, `calendar.matched`, `session.saved`,
`notes.generated`, mapped from bus topics by `events_for`. A run gets
`OATS_*` env and the bus payload on stdin. Verbs: `hook-list`, `hook-test`,
`hook-add`, `-edit`, `-remove`, `-enable`.

Gaps: a hook cannot target one meeting; nothing fires on what is said; the
payload has no schema, sequence number or dedup key; the only way to learn
what a hook would do is to run it; transcription is live or nothing.

## The tower

Each layer is a pure function of the one below. An agent reads any
layer through the control API and never has to reconstruct one from logs.

| Layer | Thing | Where it lives | How an agent reads it |
| --- | --- | --- | --- |
| L0 raw | Every bus message a session saw, with `seq` and `at` | `<session>/events.jsonl`, append-only | `session-events` |
| L1 contract | Named hook events, each a versioned JSON Schema over one envelope | one table in `hooks/events.rs`, generated to `config/hook-events.json` | `hook-events` |
| L2 trigger | `event` + `match` filter, `matches(hook, envelope) -> Verdict { fire, reason }` | `hooks.rs`, pure | `hook-explain` |
| L3 action | `command` run with env + envelope (stdin and `OATS_EVENT_FILE`) | `hooks.rs` `run` | `hook-test` |
| L4 ledger | One row per decision and per run: `run_id`, hook, event, `seq`, verdict, exit, timing, output tail | `<session>/hook-runs.jsonl` (`hooks/ledger.rs`) + the in-memory history | `hook-runs`, `hook-list` |
| L5 replay | L0 fed back through L2 (and optionally L3) | `hooks/replay.rs`, pure | `hook-simulate` (`run = false`, the default, runs nothing) |

Rules that hold across layers:

1. **One table, generated projections.** Event names, schemas, examples and
   docs come from one Rust table. `config/hook-events.json`,
   `hook-add`'s doc string, the Settings event picker and `config/schema.json`
   are generated from it (`config generate`), never hand-copied. A test fails
   when a projection drifts.
2. **Every decision is explainable.** L2 returns a reason for not firing as
   well as for firing (`match.title: "Standup*" did not match "1:1 Bob"`).
   The ledger records verdicts as well as runs.
3. **Replay over live.** The raw L0 log is enough to re-derive every L1-L4
   result. A behavior is proven by replaying a recorded session, not by
   waiting for a meeting.
4. **Append-only, per session, no locks.** `events.jsonl` and
   `hook-runs.jsonl` are one file per session written by one writer. No
   global lock, queue or concurrency cap (dotfiles no-bottlenecks ruling).
5. **Recordings never wait on hooks**, except a `blocking = true` hook on a
   `*.before_*` event. That hook is bounded by its timeout and fails open:
   a timeout or crash lets the action proceed and is logged.

## L0: the raw log

`<session>/events.jsonl` holds one line per bus message that names the
session (`session` or `id`), live transcript `segment`s included, and per
hook event an action fires itself with no bus topic (`notes.generated`,
logged under its event name):

```json
{"seq": 412, "at": "2026-11-13T15:04:05.120Z", "topic": "segment", "data": {"session": "...", "kind": "live", "text": "..."}}
```

`seq` is the envelope's `seq`: the log is the counter
(`sessions/events.rs`). Each session has its own counter behind its own
lock, taken to count and append one line, so the lines are in `seq` order
with no gap, and no lock is shared across sessions. A line is one append
of the whole object and its newline: a crash mid-write leaves at most a
torn last line, which `session-events` skips (and counts in `skipped`) and
the next run of the app cuts off before it appends. A session's first
message in a run continues from the last `seq` in its log, so `seq` stays
monotonic across restarts. `hook-test` is no bus message: its envelope
takes the session's last `seq` and the log gets nothing but the run's
`hook-finished`.

The log starts with the session's directory (`recording-started` is its
first line) and keeps every later message naming it, not only those up to
the end of finalisation: that end is no one moment (`auto-stopped`, the
`transcript.final` hook's `hook-finished` and `notes.generated` come after
the final pass), and replaying `session.saved` or `notes.generated` needs
them. Messages naming no session are in no log. The file lives in the
session directory, so trash, restore, delete, import and export carry it.

`session-events` reads it: `session` (default the newest), `topic`, and
`from` / `to` (a `seq` range, both ends included).

## L1: the envelope

```json
{
  "schema": "oats.hook-event/1",
  "event": "transcript.phrase",
  "seq": 412,
  "at": "2026-11-13T15:04:05.120Z",
  "run_id": "01JD...-hook-3",
  "dedup": "<session>:transcript.phrase:<utterance id>:<phrase>",
  "hook": "flag-pricing",
  "session": { "id": "...", "dir": "...", "title": "...", "audio": "...", "transcript": "..." },
  "calendar": { "title": "...", "start": "...", "end": "...", "attendees": ["..."], "source": "..." },
  "data": { }
}
```

`data` is per event and has its own schema. `seq` is monotonic within a
session: it counts the bus messages that name the session (`session` or
`id`), so every hook event one message stands for shares its `seq`
(`record.stop` and `transcript.final` at stop), and it is the line's `seq`
in the session's raw log (L0). A hook fires at most once per
`dedup` key (in memory, per session); `hook-test` runs are outside dedup. The
env vars stay (`OATS_EVENT`, `OATS_SESSION_*`, ...), plus `OATS_RUN_ID`,
`OATS_SEQ` and `OATS_EVENT_FILE` (the envelope written to a file, for
scripts that cannot read stdin, removed after the run).

`hook` (the hook's name, as `OATS_HOOK`) is in the envelope too: `run_id`
already makes the envelope per run, and a script reading only stdin should
not need the environment to know which hook it is (dot-4uzgol.111.2).

Dedup keys are `<session>:<event>[:<occurrence>]`, the occurrence named by
the table: once per session for `record.stop`, `record.auto_start`,
`record.auto_stop`; `data.started_at` for `record.start`; the calendar event
id for `calendar.matched` (the match at start and at stop is one); `record`
for `transcript.final` at stop and `seq` for a later transcription; `seq`
for every `record.pause`, `session.saved` and `notes.generated`;
`<utterance id>:<phrase>` for `transcript.phrase` (below). The schemas
are written in a small JSON Schema subset (`type`, `const`, `enum`,
`required`, `properties`, `additionalProperties`, `items`, `anyOf`,
`minimum`) that `hooks::events::validate` checks; a `data` schema types the
fields it lists and allows more (a payload may grow), the envelope allows
none.

## Events after this epic

| Event | Fires when | `data` adds |
| --- | --- | --- |
| `record.before_start` / `record.before_stop` | before the recorder acts; may be `blocking` | `reason` (manual, auto, calendar); `title`, `requested`, `calendar` / `by` |
| `record.start` / `.pause` / `.stop` / `.auto_start` / `.auto_stop` | as today | `reason`, durations |
| `calendar.matched` | as today | event |
| `calendar.ending` | `match.before_end` before the matched event's scheduled end, while recording | `remaining_ms`, `remaining`, `end`, `event_id` |
| `calendar.ended` | the matched event's scheduled end passes while recording | `overrun_ms`, `overrun`, `end`, `event_id` |
| `transcript.phrase` | a final (not partial) utterance matches a phrase | `utterance {id, text, speaker, start_ms, end_ms}`, `phrase`, `live`, `kind` |
| `transcript.final` | the final pass is written | utterance count, mode |
| `session.saved`, `notes.generated` | as today | as today |

"Meeting ended" for one meeting is `record.stop` (or `calendar.ended`) plus
`match.title`.

### Before the recorder acts (dot-4uzgol.111.7)

`record.before_start` and `record.before_stop` are fired by the action, not a
bus topic (the raw log keeps them under their event name), on the action's
own thread, before `Recorder::start` / `stop`:

- **`data.reason`**: `manual` for `record-start` / `record-stop` (a shortcut,
  the window, ctl), `auto` for auto-start / auto-stop. `calendar` is in the
  enum for a start or stop a calendar event makes; nothing does that yet, so
  no event carries it today. A forced stop (the recording can no longer
  write) fires no `record.before_stop`: nobody asked for it and there is
  nothing a hook could prepare. Neither fires when the action is bound to
  fail (a start while recording, a stop while idle).
- **No session yet at `before_start`.** The session directory does not exist,
  so the event names no session (`session.id` is null, the dedup key is
  `:record.before_start:<seq>`) and the session's raw log still starts with
  `recording-started` at `seq` 1. `data.title` and the envelope's
  `calendar` are what the recording will get (the calendar is matched
  first), so `match.title` / `attendees` / `calendar` work on it;
  `data.requested` is the session id asked for, if any. `before_stop`
  names the session and adds `data.by`.
- **`blocking = true`** is allowed only on these two events
  (`events::may_block`: a `*.before_*` name); `config-check` and every write
  refuse it elsewhere, naming the hook. The recorder judges every hook on the
  event (with `hook-decided`, as the tap does), starts each that fires on its
  own thread, then waits for the blocking ones: all at once (no lock, queue
  or cap), until all have ended or the longest of their timeouts (plus one
  `app.hook_poll` for a killed run to be reaped) has passed. Then it acts
  whatever happened: a timeout, a crash, a missing program or a non-zero
  exit is logged (`blocking hook X on record.before_start: exit code 3
  (logged, not enforced); waited 120ms, the recorder goes on`) and returned
  in the action's answer and its bus payload as `before_hooks`
  (`[{hook, event, waited_ms, finished, ok, exit_code, timed_out, error,
  run_id}]`, only when a blocking hook ran). Hooks that do not block are
  never waited for. While a `before_stop` hook blocks, the recording goes on.

### Calendar timers (dot-4uzgol.111.7, `hooks/ending.rs`)

The recording's watcher (the thread that already runs auto-stop, every
`record.auto_check`) also looks at the calendar, but only when some hook
listens for `calendar.ending` or `calendar.ended`: it reads the session's
`session.json` (not its transcript) for the matched `calendarEvent`, and the
pure `ending::tick(state, hooks, end, now, default)` says what is due:

- **`calendar.ending` is per hook.** A hook is due once the time left is at
  most its `match.before_end` (default `app.hook_before_end`, 5m; the same
  range bounds a hook's own), while the end is still ahead. One tick fires
  the event once for the hooks that came due on it (the others get no
  decision, as a phrase hook whose phrase was not said), each hook once per
  recording; `data.remaining_ms` is the time left at that tick (a recording
  that starts inside the window fires at once with what is left).
  `matches` explains `before_end` against `data.remaining_ms`
  (`match.before_end = 2m: 1m59s left before the scheduled end`).
- **`calendar.ended` fires once**, at the first tick at or past the end, and
  only if an earlier tick saw the end ahead (it passed while recording).
  `data.overrun_ms` is how far past the end that tick was; the recording
  going on does not fire it again. A tick that jumps the window (the machine
  slept) says only `calendar.ended`.
- Dedup keys are `<session>:calendar.ending:<event id>` and
  `<session>:calendar.ended:<event id>`. The tests drive `tick` with a fake
  clock; the engine passes the wall clock.

## L2: `match`

```toml
[[hooks]]
name = "standup-to-linear"
event = "record.stop"
command = ["~/bin/standup-sync"]
[hooks.match]
title = "Standup*"            # glob; "re:..." for a regex
attendees = ["*@acme.com"]    # any one matches
source = "google-work"        # calendar source name
calendar = true               # only calendar-matched sessions

[[hooks]]
name = "flag-pricing"
event = "transcript.phrase"
command = ["sh", "-c", "notify 'pricing came up'"]
shell = false
[hooks.match]
phrases = ["pricing", "re:\\bdiscount(s)?\\b"]
speaker = "remote"            # you | remote | any
cooldown = "2m"               # per hook per session
```

Every `match` field is optional, and all present fields must hold.

How the L2 fields read the envelope (dot-4uzgol.111.4):

- A pattern is a glob (`*`, `?`, `[abc]`, `[!abc]`), case-insensitive and
  over the whole text, or after `re:` a regex as written (unanchored,
  case-sensitive unless it says `(?i)`). `source` is a pattern too, so a
  plain name is an exact (case-insensitive) match.
- `title` matches `session.title` or, when it differs, `calendar.title`.
  `attendees` holds when any pattern matches any `calendar.attendees` entry
  (email, else name). `source` reads `calendar.source`. `calendar = false`
  means only sessions with no calendar event.
- `speaker`, `phrases` and `cooldown` are judged on `transcript.phrase`
  (below); `before_end` by `calendar.ending` (below).
- `matches` checks the event, then `enabled`, then `calendar`, `title`,
  `attendees`, `source`. A skip's reason lists every field that failed, a
  fire's every field that held (`no match filter: fires on every
  record.stop` for none), joined with `; `.
- The bus tap judges every hook on the event, disabled ones too, then dedup
  (`already fired for <dedup> (<reason>)`). Each verdict is published as
  `hook-decided` (`{at, hook, event, session, event_seq, dedup, fire,
  reason}`) and kept, newest first and `app.hook_history` long, in
  `hook-list`'s `decisions` and as each hook's `last_decision`.
- `hook-explain` judges every hook (any event) against the envelope of a
  session (default the newest) and event built from the session store; it
  runs nothing and consumes no dedup key.
- `config-check` (and every write) refuses a pattern that does not compile
  or an unknown `match` key, naming the hook: `hook standup: match.title:
  bad glob "[x": [ is never closed`.

### Phrase triggers (dot-4uzgol.111.6, `hooks/phrase.rs`)

`transcript.phrase` follows the `segment` topic: the live pass's utterances
(`kind = "live"`) and the final pass's (`"final"`), both already past echo
removal. Any other kind (a partial) is never judged, so it never fires.

- **Normalized text.** Case-folded; apostrophes dropped (`don't` is `dont`);
  every other character that is not a letter, digit or space is a word
  break (`follow-up` is `follow up`); whitespace collapsed. A literal phrase
  is normalized the same way and matches whole words (`price` is not in
  `pricing`). A `re:` phrase is a regex run on the normalized text. A phrase
  with no words or a bad regex is refused naming the hook, and a
  `transcript.phrase` hook needs at least one phrase.
- **The first phrase said.** A hook whose phrases the utterance does not
  say gets no decision (the event did not happen for it). One that says
  several fires once, for the first in its list (`data.phrase`).
- **`speaker`**: `you` is the mic side (`you`, `room_*`), `remote` everyone
  else (`remote_*`), `any` (or unset) both.
- **Utterance id** `<speaker>@<start_ms>`. When a later utterance says the
  same phrase for the same hook on the same side overlapping in time (the
  final pass re-seeing what the live pass heard, with its own boundaries
  and maybe another remote id), it takes the first one's id, so its dedup
  key `<session>:transcript.phrase:<utterance id>:<phrase>` is the same and
  one spoken phrase fires once across the live pass, the final pass and a
  replay.
- **`cooldown`** (default `app.hook_cooldown`, 30s; `0s` off), per hook per
  session, in recording time (`start_ms`), not wall time: an utterance that
  starts within the cooldown of one the hook fired on is skipped. A replay
  or a final pass run in seconds judges as the live pass did.
- **Order.** The bus tap judges each utterance on the publishing thread, in
  bus order (the session is read only when some hook's phrase is said), then
  publishes the verdicts and runs the hooks on threads; it never publishes
  from inside the tap.
- **`live`** is true for the live pass while recording; false for the final
  pass and for a file transcribed later (`session-transcribe`,
  `stt-transcribe`: their segments carry `offline = true`).
- **Reasons.** A fire: `match.speaker = remote: "remote_1" is remote;
  match.phrases: "pricing" said in "So, pricing?"`. Skips add `already fired
  for <dedup> (...)` and `match.cooldown: fired at 1:01 (recording time),
  within 2m of 1:30 (...)`. `hook-explain event=transcript.phrase
  utterance={text, speaker}` judges a given utterance without consuming any
  dedup key or cooldown.

## Transcription mode

`audio.transcribe` (the key is `audio.transcribe`, not `record.transcribe`)
gains `deferred`: no live STT while recording (no ASR is built or loaded, no
live segments), so CPU stays near capture-only. The final pass runs by itself
at stop, then `transcript.final` fires:

- `recording-started` carries `transcription.state = "deferred"`, and the
  session is marked `transcribePending` from its start, so a recording the
  app quits during is transcribed at the next launch.
- `record-stop` saves the audio and publishes `recording-stopped`
  (`transcription.final = "running"`, so no `transcript.final` from it) and
  `session-saved` (`record`), then runs the final pass on the stopping thread
  (the same place `auto` runs its own) through `session-transcribe`, final
  pass only. Its `session-saved` (`transcribed`) is the one `transcript.final`.
  `record-stop` returns the pass's result in `transcription`.
- `transcribe-progress` (`{session, mode, state}`) says `running`, then
  `done` or `failed` (with `error`). The Live view says transcription runs
  after stop while recording, and the pass's state after it.
- A failed pass (say the model is missing) keeps the audio and the pending
  mark and says why; a missing model is downloaded per
  `stt.local.auto_download` and the session transcribed when it arrives, and
  `session-transcribe` runs it any time.
- `notes-generate` for a session whose pass is running waits for it.

In `deferred` mode `transcript.phrase` hooks fire during the final pass with
`live = false`. `config-check` (and `hook-list`, and the Settings Hooks
panel) returns `warnings`: one line per enabled phrase hook that cannot fire
mid-meeting (`audio.transcribe` `deferred` or `off`, or `stt.live.enabled =
false`).

## L4: the ledger (dot-4uzgol.111.8, `hooks/ledger.rs`)

`<session>/hook-runs.jsonl` gets one row per verdict the engine makes on a
session's event (fire or skip, the `hook-decided` it publishes) and one per
run (by the event, `hook-test` or `hook-simulate run = true`), appended when
the session's directory exists (a `record.before_start` names no session, so
it is in no ledger). Every row has the same fields:

```json
{"kind": "run", "at": "...", "session": "...", "run_id": "...", "hook": "...",
 "event": "record.stop", "seq": 12, "dedup": "...", "verdict": "fire",
 "reason": "match.title: \"Standup*\" matched \"Standup\"", "outcome": "ok",
 "exit": 0, "timed_out": false, "error": null, "started": "...", "ms": 41,
 "stdout_tail": "...", "stderr_tail": "", "test": false, "simulated": false}
```

A verdict row (`kind = "verdict"`) has null run fields (its run, if any, is
the next row with the same hook and `dedup`); a run row's `reason` is the
verdict that fired it. `outcome` is `fire` or `skip` for a verdict, `ok`,
`failed` or `timed_out` for a run. The output tails are the last
`app.hook_ledger_tail` bytes of what the run kept (`app.hook_output`). The
writer is as the raw log's: one append of a whole line under the session's
own lock, so a crash leaves at most a torn last line, which `hook-runs`
skips and counts in `skipped`. Like `events.jsonl` it lives in the session
directory, so trash, restore and delete carry it.

`hook-runs` reads it: `session` (default every session, merged by `at`),
`hook`, `event`, `outcome`, `limit` (the newest that many rows).

## L5: replay (dot-4uzgol.111.8, `hooks/replay.rs`)

`replay::simulate(hooks, lines, base, opts)` is pure. Each L0 line stands for
the events `events_for` maps its topic to, or for the event it is named
after (`notes.generated`, `calendar.ending`, `record.before_stop`: an
action fired it and the log keeps it under its name, its dedup key formed
as `fire` forms it). Every hook on the event is judged, disabled ones too,
exactly as the tap does: `transcript.phrase` through `phrase::judge` (a
fresh `State`, so cooldowns and the live-to-final dedup run in recording
time as they did live), `calendar.ending` only for the hooks that came due on
that tick (`match.before_end`, else `app.hook_before_end`, at least the
line's `remaining_ms`; once each per replay), the rest through `matches`, then
dedup in the replay's own set (it starts empty and consumes nothing of the
engine's). The session is the store's (folder, files, title, calendar) when
it knows the id, else what the log says (`recording-started`'s title and
calendar event).

`hook-simulate` runs it over a session's `events.jsonl` (default the newest)
or `file` (any `events.jsonl`; its lines name the session) and returns
`decisions` (every verdict in order: `seq`, `topic`, `event`, `hook`,
`fire`, `reason`, `dedup`) and `fires` (each with the envelope it would
get, `run_id` `simulated-<seq>-<hook>`). `hook` judges just that one;
`enable = true` judges every hook as if it were on (the agent loop: add the
hook with `enabled = false`, simulate, then `hook-enable`). `run = false`
(the default) runs and writes nothing. `run = true` runs each fire in order
and waits for it, as `hook-test` does (so the run's own `hook-finished` lands
in the raw log, as `hook-test`'s does), adds its result to the fire as
`run`, and writes every verdict and run to the session's ledger with
`simulated = true`.

The replay tests (`crates/core/tests/hook_replay.rs`) feed the fixtures in
`crates/core/tests/fixtures/hook-replay/` (a calendar-matched meeting with
phrases, a manual recording that auto-stops, a deferred-transcription
session) through `simulate` and assert the exact ordered verdict list, the
reasons and dedup keys, and that every fire's envelope holds to its schema.

## Agent surface (all in `config/actions.json`)

| Verb | Returns |
| --- | --- |
| `hook-events` | every event: description, `data` schema, an example envelope, which bus topic produces it |
| `hook-explain` | for a session (default newest) and an event: each hook, fire or not, and why (`utterance` for `transcript.phrase`) |
| `hook-simulate` | replays `<session>/events.jsonl` (or a given `file`) through L2; `run = true` also runs L3 and ledgers it as `simulated`; returns every verdict in order and the fires with their envelopes; `hook`, `enable` |
| `hook-runs` | the ledger, filterable by `session`, `hook`, `event`, `outcome` (`fire`, `skip`, `ok`, `failed`, `timed_out`), `limit` |
| `session-events` | the raw L0 log of a session (default the newest), filtered by `topic` and a `from`..`to` `seq` range |
| `subscribe topics=["hook-finished", "hook-decided"]` | live stream (already exists) |

An agent's loop is `hook-events`, write the hook (`hook-add` with
`enabled = false`), `hook-simulate hook=<it> enable=true` on a real past
session (`run = true` to see its command work on that session's envelopes),
`hook-enable`, and later `hook-runs hook=<it>` for what it did. It never
needs a live meeting to know what will happen.

## Build cache

The repo gets a `.cargo/config.toml` whose `rustc-wrapper` is
`ci/rustc-wrapper.sh`. The wrapper uses kache when it is on PATH (the
dotfiles default, `KACHE_LOCAL_ONLY=1`) and passes through otherwise. Every
worktree and pod then shares compiled dependencies. `RUST_CACHE=sccache`
selects sccache and `RUST_CACHE=none` neither; a tool that cannot start also
passes through, so the cache never fails a build. `ci/rustc-wrapper-test.sh`
tests it (run by `ci/linux-nix.sh`).

## What proves it

1. Replay tests: recorded `events.jsonl` fixtures in `crates/core/tests/`
   fed through `hook-simulate`, asserting the exact ordered fire list and
   verdict reasons.
2. Property tests: one phrase in one final utterance fires once; partials
   never fire; cooldown holds; dedup holds across a replay.
3. A projection-drift test: `config/hook-events.json` equals the table.
4. CPU: the same 30-minute recording on carbon in `auto`, `off` and
   `deferred`, measured with `powermetrics` and `/usr/bin/time -l`; also
   time-to-`transcript.final`.
5. Transcript parity: the final transcript in `deferred` mode matches
   `auto`'s final pass on the same audio (same model, same words).
6. One gate run on `feat/hooks-v2` (`bash ci/linux-nix.sh`) green before the
   merge to `main`.

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
| L1 contract | Named hook events, each a versioned JSON Schema over one envelope | one table in `hooks.rs`, generated to `config/hook-events.json` | `hook-events` |
| L2 trigger | `event` + `match` filter, `matches(hook, envelope) -> Verdict { fire, reason }` | `hooks.rs`, pure | `hook-explain` |
| L3 action | `command` run with env + envelope (stdin and `OATS_EVENT_FILE`) | `hooks.rs` `run` | `hook-test` |
| L4 ledger | One row per decision and per run: `run_id`, hook, event, `seq`, verdict, exit, timing, output tail | `<session>/hook-runs.jsonl` + the in-memory history | `hook-runs`, `hook-list` |
| L5 replay | L0 fed back through L2 (and optionally L3) | `hook-simulate` | `hook-simulate --dry-run` |

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

## L1: the envelope

```json
{
  "schema": "oats.hook-event/1",
  "event": "transcript.phrase",
  "seq": 412,
  "at": "2026-11-13T15:04:05.120Z",
  "run_id": "01JD...-hook-3",
  "dedup": "<session>:transcript.phrase:<utterance id>:<phrase>",
  "session": { "id": "...", "dir": "...", "title": "...", "audio": "...", "transcript": "..." },
  "calendar": { "title": "...", "start": "...", "end": "...", "attendees": ["..."], "source": "..." },
  "data": { }
}
```

`data` is per event and has its own schema. `seq` is monotonic within a
session. A hook fires at most once per `dedup` key. The env vars stay
(`OATS_EVENT`, `OATS_SESSION_*`, ...), plus `OATS_RUN_ID`, `OATS_SEQ` and
`OATS_EVENT_FILE` (the envelope written to a file, for scripts that cannot
read stdin).

## Events after this epic

| Event | Fires when | `data` adds |
| --- | --- | --- |
| `record.before_start` / `record.before_stop` | before the recorder acts; may be `blocking` | `reason` (manual, auto, calendar) |
| `record.start` / `.pause` / `.stop` / `.auto_start` / `.auto_stop` | as today | `reason`, durations |
| `calendar.matched` | as today | event |
| `calendar.ending` | `match.before_end` before the matched event's scheduled end, while recording | `remaining` |
| `calendar.ended` | the matched event's scheduled end passes while recording | `overrun` |
| `transcript.phrase` | a final (not partial) utterance matches a phrase | utterance, speaker, matched phrase, `live` |
| `transcript.final` | the final pass is written | utterance count, mode |
| `session.saved`, `notes.generated` | as today | as today |

"Meeting ended" for one meeting is `record.stop` (or `calendar.ended`) plus
`match.title`.

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

Every `match` field is optional, and all present fields must hold. Phrase
matching runs on normalized text (case-folded, punctuation stripped) of
final utterances that survived echo removal. It never matches partials, so
one spoken word fires once.

## Transcription mode

`record.transcribe` gains `deferred`: no live STT while recording, so CPU
stays near capture-only. The final pass runs by itself at stop, then
`transcript.final` fires. In `deferred` mode `transcript.phrase` hooks fire
during the final pass with `live = false`. `config-check` warns that they
cannot fire mid-meeting.

## Agent surface (all in `config/actions.json`)

| Verb | Returns |
| --- | --- |
| `hook-events` | every event: description, `data` schema, an example envelope, which bus topic produces it |
| `hook-explain` | for a session (default newest) and an event: each hook, fire or not, and why |
| `hook-simulate` | replays `<session>/events.jsonl` (or a given file) through L2; `run = true` also runs L3; returns the ordered fire list |
| `hook-runs` | the ledger, filterable by session, hook, event, outcome |
| `session-events` | the raw L0 log of a session |
| `subscribe topics=["hook-finished", "hook-decided"]` | live stream (already exists) |

An agent's loop is `hook-events`, write the hook, `hook-simulate` on a real
past session, `hook-enable`. It never needs a live meeting to know what will
happen.

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

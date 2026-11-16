oats-meetings-rs: GPUI + Rust rewrite of openoats.

## Install (macOS, Apple silicon)

```sh
brew install --cask davidawad/tap/oats-meetings
```

The cask installs `Oats Meetings.app` and links its CLI as `oats`. The app is ad-hoc signed, not
notarized, so the cask clears the quarantine flag after install. Releases:
https://github.com/davidawad/oats-meetings-rs/releases

Licensed GPL-3.0-or-later (see `LICENSE`). No cargo on carbon: build in pods.
CI: none hosted. Pods are the CI: `bin/pod-run <repo> <ref> bash ci/linux-nix.sh` (add `env CROSS_TARGET=x86_64-pc-windows-msvc` for the Windows check).

Builds go through `ci/rustc-wrapper.sh` (`.cargo/config.toml`): kache when it is installed
(`RUST_CACHE=sccache` for sccache, `RUST_CACHE=none` for neither), plain rustc otherwise. On Windows
without `sh`, build with `RUSTC_WRAPPER=""`. See [AGENTS.md](AGENTS.md#build-cache-kache-through-cirustc-wrappersh).

## Control

Every action is reachable from `oats-meetings-rs ctl` and, as MCP tools, from `oats-meetings-rs mcp`
(stdio). See [docs/control.md](docs/control.md).

## Auto-stop and auto-start

A recording stops by itself after `record.auto_stop.after` (default 5 minutes) of silence on both
system audio and the mic, with the trailing silence cut from the saved audio and transcript; it
warns first with a Keep recording button (`ctl record-keep`). Optionally (`record.auto_start.enabled`)
sustained speaker or mic sound starts one. See [docs/auto-record.md](docs/auto-record.md).

## Calendar

Recordings are titled from the calendar event that overlaps them. Events come from
`calendar.sources`: this computer's accounts, Google, Microsoft 365, CalDAV, ICS feeds, or any
program you name (`kind=command`). See [docs/calendar-providers.md](docs/calendar-providers.md).

A command source needs no sign-in. Point it at whatever already prints your calendar, as ICS or as
JSON events (Google Calendar API, Microsoft Graph, or plain `{title, start, end, uid, location,
attendees}`). It runs on every calendar read, without a shell:

```sh
#!/bin/sh
# ~/bin/my-meetings: any tool that prints events works; this one prints plain JSON.
cat <<JSON
[{"title": "Design review", "start": "2026-11-09T10:00:00+01:00",
  "end": "2026-11-09T10:30:00+01:00", "uid": "review-42@example.com",
  "attendees": ["ana@example.com"]}]
JSON
```

```sh
ctl calendar-source-add name=mine kind=command 'argv=["/home/me/bin/my-meetings"]'
ctl calendar-connect source=mine      # a test run: how many events it read, or why it failed
ctl calendar-events
```

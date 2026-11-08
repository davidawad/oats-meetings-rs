# Calendar providers

oats-meetings-rs titles a recording from the calendar event that overlaps it (dot-4uzgol.88). Events
come from **calendar sources** (`calendar.sources`). Each source is read through one **provider**
(dot-4uzgol.100). Every provider lists accounts, then the calendars in each account. You tick
calendars the same way for all of them: the Settings > Calendar sources checkboxes, or
`ctl calendar-select`.

| kind        | reads                                                                                   | network          | sign-in                                   |
|-------------|------------------------------------------------------------------------------------------|------------------|-------------------------------------------|
| `os-local`  | this computer's accounts: macOS Calendar (EventKit, whatever Internet Accounts holds: iCloud, Google, Exchange, CalDAV); Linux Evolution Data Server / GNOME Online Accounts and KDE Akonadi; Windows Appointments | none | macOS: the Calendars grant (asked once) |
| `google`    | Google Calendar API v3                                                                   | `network.allow`  | OAuth, installed-app flow (loopback + PKCE) |
| `microsoft` | Microsoft Graph `/me/calendars/{id}/calendarView` (Microsoft 365, Outlook.com)          | `network.allow`  | OAuth, loopback + PKCE                    |
| `caldav`    | any CalDAV server: iCloud, Fastmail, Nextcloud, Proton Bridge, Radicale, ...             | `network.allow`  | user name + app password (`secret:` ref)  |
| `ics`       | the cached `ics.feeds`: URLs (`https`, `webcal`, `caldav://`) and local `.ics` files      | the feed refresh | the feed's header secret                  |
| `eventkit`  | the macOS Calendar app (the old name for os-local on a Mac)                              | none             | the Calendars grant                       |
| `command`   | whatever a program you name prints (ICS, or Google / Graph / plain JSON events), run on each read | the program's own | none (the program's own)          |

The default is `[{"name":"local","kind":"os-local"},{"name":"ics","kind":"ics"}]`, so the
accounts already on the computer are used with no setup.

The matching and title rules (`calendar.match_lead`, `ignore_*`, `prefer_*`, `title_template`) apply
to every provider. Two sources can show the same meeting, for example a Google account seen
through both macOS Internet Accounts and a `google` source. Those events share an iCalendar UID
(Google `iCalUID`, Graph `iCalUId`, EventKit's external identifier). An event with the same UID and
start as one already read is dropped, so the source listed earlier in `calendar.sources` wins.

## Add a source

```sh
# This computer's accounts (the default).
ctl calendar-source-add name=local kind=os-local

# Google: a Desktop-app OAuth client from Google Cloud Console (Calendar API enabled).
ctl secret-set google/me <client secret>
ctl calendar-source-add name=me kind=google client_id=<id>.apps.googleusercontent.com client_secret=secret:google/me
ctl config-set network.allow true
ctl calendar-connect source=me open=true     # sign in in the browser

# Microsoft 365 / Outlook.com: an Entra app registration (public client, redirect http://localhost).
ctl calendar-source-add name=work kind=microsoft client_id=<application id> tenant=organizations
ctl calendar-connect source=work open=true

# CalDAV: discovery starts from the server URL (.well-known/caldav, then the principal and home).
ctl secret-set caldav/fastmail <app password>
ctl calendar-source-add name=fastmail kind=caldav url=https://caldav.fastmail.com/ username=me@example.com password=secret:caldav/fastmail
ctl calendar-connect source=fastmail         # lists the calendars it found
```

`ctl calendar-source-edit name=<source> ...` changes the given fields of a source later.

Settings has the same steps: **Use this computer's calendars**, **Add Google**, **Add Microsoft
365**, then **Connect** on the new source. The `calendar.google_client_id` and
`calendar.microsoft_client_id` keys give a default client to sources that set none.

## Command sources

A `command` source (dot-4uzgol.105) runs a program on every calendar read and takes the events it
prints. Use it for any feed you already have: a calendar CLI, a script over an API, a file another
tool keeps up to date. Nothing in oats knows about the program: its argv is user config.

```sh
ctl calendar-source-add name=mine kind=command 'argv=["my-calendar","events","--json"]' \
    'env={"MY_CALENDAR_PROFILE":"work"}' cwd=/home/me timeout=30s format=auto
ctl calendar-connect source=mine              # run it now: events read, or the error
ctl calendar-source-edit name=mine format=json
```

- `argv` (required): the program and its arguments, an array (a string is split on spaces). It is
  never run through a shell; for pipes, name a script.
- `cwd`: its working directory (default the app's). `env`: variables added to the app's
  environment, an object or `NAME=value` strings.
- `timeout`: how long it may run (default `calendar.command_timeout`, 20 s); it is then killed,
  with its process group on unix.
- `format`: `auto` (default: `BEGIN:VCALENDAR` is ICS, `[` or `{` is JSON), `ics` or `json`.

What it may print:

- **ICS**: one VCALENDAR, read with the `ics.*` parser (recurrences expanded, `ics.default_tz`).
- **JSON**: an array of events, or an object with the array under `items`, `events` or `value`.
  Each event is one of:
  - a Google Calendar API event: `summary`, `start.dateTime` or `start.date`, `end`, `iCalUID`,
    `id`, `location`, `attendees[].email`, `status` (`cancelled` events are dropped);
  - a Microsoft Graph event: `subject`, `start.dateTime` + `start.timeZone`, `iCalUId`,
    `isCancelled`;
  - a plain event: `title` or `summary`, `start`, `end` (RFC 3339, or `YYYY-MM-DD` for all-day),
    and optionally `uid`, `location`, `attendees` (addresses or `{email, name}`), `url`,
    `description`, `status`.

Stdout is capped at `calendar.command_max_bytes` (8 MB). A run that cannot start, exits nonzero
(`command_failed`, with the start of its stderr), runs past its timeout (`command_timeout`), or prints
too much or something unreadable (`command_output`) is that source's error. It shows in
`calendar-events`, `calendar-sources-list` (`error`, and `last_run`) and on the source's row in
Settings. It never stops a recording, and the events of the source's last good run are still used.
Events dedupe with the other sources by iCalUID and start, and a matched event names the session as
with any other provider.

In Settings, **Add command feed** opens a form (name, command, format, timeout, folder, env); a
command source's row shows its command and last run, with **Run now** and **Edit**.

## Sign-in and tokens

`calendar-connect` binds `127.0.0.1` on a free port and returns the authorization URL, which Settings
opens in the browser. It then waits up to `calendar.oauth_timeout` for the browser to come back with
the code. The code is exchanged for tokens with the PKCE verifier. The bus then carries
`calendar-connected` (with the account address) or `calendar-connect-failed`.

The tokens are kept in the app's own secret store: `calendar.token_file`, default
`calendar-tokens.json` beside the user config, mode 0600. They are never put in the OS keychain,
never written to the config, and never returned by an action. An access token within
`calendar.token_skew` of expiry is refreshed before use. `calendar-disconnect` forgets the
tokens. CalDAV passwords and Google client secrets are `secret:` references, resolved through
`secrets.backend`. A plaintext value is refused.

## os-local per platform

- **macOS**: EventKit. Turning the source on, or `calendar-access request=true`, asks for the
  Calendars grant once. Listing and matching never prompt.
- **Linux**: no permission and no network. Evolution Data Server sources are read from
  `$XDG_CONFIG_HOME/evolution/sources/*.source`. This covers GNOME Calendar, Evolution, and every
  GNOME Online Accounts account with Calendar on, each shown as its own account. Events come from:
  - local calendars: `$XDG_DATA_HOME/evolution/calendar/<uid>/calendar.ics`;
  - synced calendars: EDS's offline cache, `$XDG_CACHE_HOME/evolution/calendar/<uid>/cache.db`
    (read-only SQLite).

  KDE Akonadi's iCal file and folder resources (`akonadi_ical_resource_*rc`,
  `akonadi_icaldir_resource_*rc`) are read too. Akonadi's DAV accounts keep events in its own
  database: add those servers as a `caldav` source.
- **Windows**: `Windows.ApplicationModel.Appointments`, read-only, covering the accounts in
  Settings > Accounts. If this process cannot open the store (no package identity, or the Calendar
  privacy switch is off), the source reports `calendar_access` with the reason.

## Keys

`calendar.timeout` (one HTTP call), `calendar.oauth_timeout`, `calendar.token_skew`,
`calendar.page_size` / `calendar.max_pages` (Google / Graph paging; CalDAV redirect hops),
`calendar.token_file`, `calendar.google_*` and `calendar.microsoft_*` (client ids, tenant, endpoint
URLs), `calendar.command_timeout` and `calendar.command_max_bytes` (command sources).

## Tests

`crates/core/tests/calendar_providers.rs` covers:
- the provider trait with fakes: listing, checkboxes, `network.allow`, iCalUID dedupe, and a
  recording titled from an os-local event;
- Google and Graph request shaping against recorded replies in `crates/core/tests/fixtures/calendar`;
- the OAuth loopback + PKCE flow and token refresh against a fake token endpoint;
- CalDAV discovery against a fake server, and against a real Radicale when `radicale` is on PATH.
  CI's nix environment provides one;
- the Linux reader over a fake Evolution / Akonadi home.

`crates/core/tests/calendar_command.rs` runs fixture scripts through a command source end to end:
Google-shaped JSON, ICS, plain JSON, dedupe, a recording titled from it, and nonzero exit,
timeout, oversized and unreadable output keeping the last good events. The JSON parsers have unit
tests over synthetic events in `crates/core/src/calendar/command.rs`.

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

Settings has the same steps: **Use this computer's calendars**, **Add Google**, **Add Microsoft
365**, then **Connect** on the new source. The `calendar.google_client_id` and
`calendar.microsoft_client_id` keys give a default client to sources that set none.

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
URLs).

## Tests

`crates/core/tests/calendar_providers.rs` covers:
- the provider trait with fakes: listing, checkboxes, `network.allow`, iCalUID dedupe, and a
  recording titled from an os-local event;
- Google and Graph request shaping against recorded replies in `crates/core/tests/fixtures/calendar`;
- the OAuth loopback + PKCE flow and token refresh against a fake token endpoint;
- CalDAV discovery against a fake server, and against a real Radicale when `radicale` is on PATH.
  CI's nix environment provides one;
- the Linux reader over a fake Evolution / Akonadi home.

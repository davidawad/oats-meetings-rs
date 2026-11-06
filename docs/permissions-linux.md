# Permissions on Linux (dot-4uzgol.96)

`ctl perms-status` lists every permission the app needs, its state and how it was obtained.
`ctl perms-ensure` acquires whatever can be acquired without a person and reports the status
again. Neither ever opens a dialog. `all_granted` is true when every row is `granted`,
`not_needed` or `disabled`.

| Row | How it is obtained | No-human path (`perms-ensure`) |
| --- | --- | --- |
| `microphone` | A client of the user's PipeWire / PulseAudio server (`pactl`+`parec`, or `pw-dump`+`pw-record`). A process of the user's session needs no portal and gets no prompt. | Starts the user audio server if it is down: `systemctl --user start pipewire.socket pipewire-pulse.socket wireplumber.service`, else `pulseaudio --start`, then waits `audio.ensure_wait`. |
| `system_audio` | The monitor source of a sink. No ScreenCast portal. | As above. If the server has no sink to monitor (a headless box), loads `module-null-sink sink_name=oats_meetings_null` (or a `support.null-audio-sink` node through `pw-cli`). Set `audio.ensure_null_sink = false` to leave the audio graph alone. |
| `screencast_portal` | Not used. Nothing calls the xdg-desktop-portal ScreenCast or Camera interface, so there is no dialog and no `restore_token` / `persist_mode` to keep. | n/a |
| `calendar` | ics feeds and CalDAV collections (`ics-add work caldav://host/path`) are plain file and HTTP reads, with no OS permission. They are gated only by `network.allow`, `ics.allow_file` and the feed's header secret. | Nothing: policy keys and secrets are the user's choice. `blocked` lists each feed's problem and the `ctl` command that fixes it. |

## Headless sessions (ssh, cron)

A client finds the user's audio server through `XDG_RUNTIME_DIR` (or `PULSE_SERVER`). When both
are unset, `perms-status` says so in the `fix`. Export `XDG_RUNTIME_DIR=/run/user/$(id -u)`, and
run `loginctl enable-linger $USER` once so the user's PipeWire runs without a graphical login.

## CalDAV credentials

The credentials are a feed header that points at a secret. They never go into the config file:

```sh
oats-meetings-rs ctl secret-set caldav/work "Basic $(printf '%s' 'user:app-password' | base64)"
oats-meetings-rs ctl ics-add work caldav://dav.example.com/calendars/me/work/ \
  header='Authorization: secret:caldav/work'
oats-meetings-rs ctl config-set network.allow true
oats-meetings-rs ctl ics-refresh work
```

`caldav://` is https. `caldav+http://` is plain http, for a loopback server.

## Flatpak

The audio socket comes from the manifest: `--socket=pulseaudio`, and `--share=network` for http
and CalDAV feeds. Without the socket, both audio rows are `denied` and the fix names the override.
`perms-ensure` tries these through `flatpak-spawn --host`. That only works when the app may talk
to `org.freedesktop.Flatpak`. Otherwise the step is `needs_human` and carries the exact command:

```sh
flatpak override --user --socket=pulseaudio <app-id>          # then restart the app
flatpak permission-set devices microphone <app-id> yes        # Device portal pre-grant
```

## Snap

The microphone and the monitor need the `audio-record` interface. Only root can connect it:
`sudo snap connect <snap>:audio-record`. `perms-ensure` reports this as `needs_human` when
`snapctl is-connected audio-record` fails.

## macOS

Mic and system audio are TCC grants and the calendar is EventKit (dot-4uzgol.88). `perms-status`
reads them without prompting. Only a person can grant them, so `perms-ensure` lists them as
`needs_human`.

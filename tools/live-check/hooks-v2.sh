#!/usr/bin/env bash
# Live check for hooks v2 (dot-4uzgol.111.9) on a real meeting, macOS.
#
#   tools/live-check/hooks-v2.sh start <auto|off|deferred> [title glob]   before the meeting
#   tools/live-check/hooks-v2.sh stop                                       after it ends
#
# start: sets audio.transcribe, installs three live-check hooks (a record.stop hook matched on
# the meeting title, a transcript.phrase hook on "oats check", a record.before_start hook) that
# append their envelope to $OUT/hooks.log, and samples the app's CPU and memory every 5 s
# (plus `sudo powermetrics` energy when sudo works without a prompt).
# stop: ends the sampling and writes $OUT/summary-<mode>.txt: mean/peak CPU, which hooks fired,
# hook-explain and hook-simulate for the newest session (simulate must agree with what fired).
# Say "oats check" once during the meeting. Run it once per mode to compare CPU.
set -euo pipefail

OATS=${OATS:-"/Applications/Oats Meetings.app/Contents/MacOS/oats-meetings-rs"}
OUT=${OUT:-"$HOME/oats-live-check"}
STATE=$OUT/.state
mkdir -p "$OUT"
ctl() { "$OATS" ctl --json "$@"; }

HERE=$(cd "$(dirname "$0")" && pwd)
hook() { # <name> <event> <match json>
  ctl hook-remove hook="$1" > /dev/null 2>&1 || true
  ctl hook-add "{\"name\":\"$1\",\"event\":\"$2\",\"timeout\":\"10s\",\"match\":$3,\"command\":[\"$HERE/log-hook.sh\",\"$OUT\"]}" > /dev/null
}

case "${1:-}" in
  start)
    mode=${2:?mode: auto, off or deferred}
    title=${3:-*}
    ctl config-set key=audio.transcribe value="$mode" > /dev/null
    hook live-check-stop record.stop "{\"title\":\"$title\"}"
    hook live-check-phrase transcript.phrase '{"phrases":["oats check"],"speaker":"any"}'
    hook live-check-before record.before_start '{}'
    (while sleep 5; do
      pid=$(pgrep -f 'MacOS/oats-meetings-rs$' | head -1) || continue
      ps -o %cpu=,rss= -p "$pid" | awk -v t="$(date +%s)" '{print t, $1, $2}'
    done) >> "$OUT/cpu-$mode.txt" &
    echo "mode=$mode cpu_pid=$!" > "$STATE"
    if sudo -n true 2> /dev/null; then
      sudo -n powermetrics --samplers tasks --show-process-energy -i 5000 -o "$OUT/powermetrics-$mode.txt" > /dev/null 2>&1 &
      echo "pm_pid=$!" >> "$STATE"
    else
      echo "no passwordless sudo: CPU via ps only (powermetrics skipped)"
    fi
    echo "live check started (mode $mode, title '$title'); record the meeting, say 'oats check' once, then: $0 stop"
    ;;
  stop)
    cpu_pid='' pm_pid='' mode=''
    # shellcheck disable=SC1090
    source "$STATE"
    kill "$cpu_pid" 2> /dev/null || true
    [ -z "${pm_pid:-}" ] || sudo -n kill "$pm_pid" 2> /dev/null || true
    s=$OUT/summary-$mode.txt
    {
      echo "mode $mode  $(date -u +%FT%TZ)"
      awk '{n++; c+=$2; if ($2>p) p=$2; r=$3} END {if (n) printf "cpu mean %.1f%% peak %.1f%% over %d samples; rss %d MB\n", c/n, p, n, r/1024}' "$OUT/cpu-$mode.txt"
      [ ! -s "$OUT/powermetrics-$mode.txt" ] || echo "powermetrics (per-task energy impact): $OUT/powermetrics-$mode.txt"
      echo "--- hooks fired"; cat "$OUT/hooks.log" 2> /dev/null || echo none
      for ev in record.stop transcript.phrase record.before_start; do
        echo "--- hook-explain $ev"; ctl hook-explain event="$ev" || true
      done
      echo "--- hook-simulate (dry run, newest session)"; ctl hook-simulate || true
    } > "$s"
    rm -f "$STATE"
    echo "wrote $s"
    ;;
  *)
    sed -n '2,13p' "$0"
    exit 2
    ;;
esac

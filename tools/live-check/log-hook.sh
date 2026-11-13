#!/bin/sh
# Called by the live-check hooks (hooks-v2.sh): appends one line and the event envelope to $1.
printf '%s %s %s\n' "$(date -u +%FT%TZ)" "$OATS_EVENT" "$OATS_TITLE" >> "$1/hooks.log"
cat "$OATS_EVENT_FILE" >> "$1/envelopes.jsonl"
echo >> "$1/envelopes.jsonl"

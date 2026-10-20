#!/bin/bash
# Claude Code Stop / SubagentStop hook: tells the app how many background tasks (subagents and
# shells; monitors never count) the session still has in flight, so its sidebar row stays
# "working" after the turn ends instead of showing "done" while agents run.
#
# Claude Code puts the live list into `background_tasks` on Stop and SubagentStop
# (https://code.claude.com/docs/en/hooks#stop-input). A missing field means "unknown", so
# nothing is sent. Requires bash, jq and perl. Always exits 0 and never blocks the stop.
# Set WARP_HOOK_LOG to a file path to log one line per call.

source "$(dirname "$0")/warp-emit.sh"
# Taken first: hooks of one session run side by side, and the app keeps the newest start, so a
# slow hook (a long transcript to scan) can't overwrite a newer count.
started_ms=$(perl -MTime::HiRes=time -e 'printf "%d", time * 1000')
INPUT=$(cat)

log() {
  [ -n "${WARP_HOOK_LOG:-}" ] && printf '%s %s\n' "$(date '+%F %T')" "$*" >> "$WARP_HOOK_LOG"
  return 0
}

has_field=$(printf '%s' "$INPUT" | jq -r 'has("background_tasks")' 2>/dev/null)
event=$(printf '%s' "$INPUT" | jq -r '.hook_event_name // "?"' 2>/dev/null)
session_id=$(printf '%s' "$INPUT" | jq -r '.session_id // ""' 2>/dev/null)

if [ "$has_field" != "true" ]; then
  log "$event session=${session_id:0:8} background_tasks=absent"
  exit 0
fi

# Monitors are long-lived watchers that many sessions keep for good; counting them would keep
# finished rows "working" forever, so they never count. Shells do: builds and tests end on their
# own; a dev server keeps its row "working" until it stops.
# Some Claude Code versions list a monitor as type "shell" with nothing that tells it apart, so
# the monitor task ids come from the transcript: a Monitor tool result carries
# toolUseResult.taskId.
transcript=$(printf '%s' "$INPUT" | jq -r '.transcript_path // ""' 2>/dev/null)
monitor_ids='[]'
if [ -n "$transcript" ] && [ -f "$transcript" ]; then
  monitor_ids=$(grep -F 'Monitor' "$transcript" 2>/dev/null | jq -sc '
    (map(.message.content? // [] | if type == "array" then .[] else empty end
         | select(.type? == "tool_use" and .name? == "Monitor") | .id)) as $uses
    | [.[] | select(.toolUseResult.taskId? != null)
         | select([.message.content? // [] | if type == "array" then .[] else empty end
                   | .tool_use_id? | select(. != null) | IN($uses[])] | any)
         | .toolUseResult.taskId]' 2>/dev/null)
  [ -n "$monitor_ids" ] || monitor_ids='[]'
fi
running='[.background_tasks[]? | select((.status // "running") == "running") | select(.type != "monitor") | select((.id // "") as $id | ($monitors | any(. == $id)) | not)]'
count=$(printf '%s' "$INPUT" | jq --argjson monitors "$monitor_ids" "$running | length" 2>/dev/null)
kinds=$(printf '%s' "$INPUT" | jq -c --argjson monitors "$monitor_ids" "$running | map(.type) | group_by(.) | map({(.[0]): length}) | add // {}" 2>/dev/null)
log "$event session=${session_id:0:8} running=${count:-?} kinds=${kinds:-{\}}"

body=$(jq -nc --arg sid "$session_id" --argjson count "${count:-0}" --argjson kinds "${kinds:-{\}}" \
  --argjson started "$started_ms" \
  '{v: 1, agent: "claude", event: "background_tasks", session_id: $sid, count: $count, kinds: $kinds, started_ms: $started}')
warp_emit "$body"
exit 0

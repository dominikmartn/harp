#!/bin/bash
# Harp: Stop / SubagentStop hook that tells the sidebar how much background work (subagents and
# shells; monitors never count) the session still has in flight, so its row keeps the background
# dot after the turn ends instead of only showing "done".
#
# Claude Code puts the live list into `background_tasks` on Stop and SubagentStop. Monitors are
# watchers that only end when stopped, so they are left out by type and by the task ids the
# transcript's Monitor tool calls returned (older Claude Code versions listed them as "shell").

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/should-use-structured.sh"

if ! should_use_structured; then
    exit 0
fi

source "$SCRIPT_DIR/build-payload.sh"

STARTED_MS=$(perl -MTime::HiRes=time -e 'printf "%d", time * 1000' 2>/dev/null)
INPUT=$(cat)

# Without the field (older Claude Code) Harp keeps its own reading.
[ "$(echo "$INPUT" | jq -r 'has("background_tasks")' 2>/dev/null)" = "true" ] || exit 0

TRANSCRIPT_PATH=$(echo "$INPUT" | jq -r '.transcript_path // empty' 2>/dev/null)
MONITOR_IDS='[]'
if [ -n "$TRANSCRIPT_PATH" ] && [ -f "$TRANSCRIPT_PATH" ]; then
    MONITOR_IDS=$(grep -F 'Monitor' "$TRANSCRIPT_PATH" 2>/dev/null | jq -sc '
        (map(.message.content? // [] | if type == "array" then .[] else empty end
             | select(.type? == "tool_use" and .name? == "Monitor") | .id)) as $uses
        | [.[] | select(.toolUseResult.taskId? != null)
             | select([.message.content? // [] | if type == "array" then .[] else empty end
                       | .tool_use_id? | select(. != null) | IN($uses[])] | any)
             | .toolUseResult.taskId]' 2>/dev/null)
    [ -n "$MONITOR_IDS" ] || MONITOR_IDS='[]'
fi

RUNNING='[.background_tasks[]? | select((.status // "running") == "running") | select(.type != "monitor") | select((.id // "") as $id | ($monitors | any(. == $id)) | not)]'
COUNT=$(echo "$INPUT" | jq --argjson monitors "$MONITOR_IDS" "$RUNNING | length" 2>/dev/null)
KINDS=$(echo "$INPUT" | jq -c --argjson monitors "$MONITOR_IDS" "$RUNNING | map(.type) | group_by(.) | map({(.[0]): length}) | add // {}" 2>/dev/null)

BODY=$(build_payload "$INPUT" "background_tasks" \
    --argjson count "${COUNT:-0}" \
    --argjson kinds "${KINDS:-{\}}" \
    --argjson started_ms "${STARTED_MS:-0}")

"$SCRIPT_DIR/warp-notify.sh" "warp://cli-agent" "$BODY"

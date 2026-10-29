#!/bin/bash
# Hook script for Claude Code PostToolUse event
# Sends a structured Warp notification after a tool call completes,
# transitioning the session status from Blocked back to Running.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/should-use-structured.sh"

# No legacy equivalent for this hook
if ! should_use_structured; then
    exit 0
fi

source "$SCRIPT_DIR/build-payload.sh"

# Read hook input from stdin
INPUT=$(cat)

TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)

# Harp: the tool's target (command or file) too, so Harp can tell which open permission dialog a
# finished tool answers when background agents run the same tool.
TOOL_TARGET=$(echo "$INPUT" | jq -c '(.tool_input // {}) | {command, file_path} | with_entries(select(.value != null))' 2>/dev/null)
[ -z "$TOOL_TARGET" ] && TOOL_TARGET='{}'

BODY=$(build_payload "$INPUT" "tool_complete" \
    --arg tool_name "$TOOL_NAME" \
    --argjson tool_input "$TOOL_TARGET")

"$SCRIPT_DIR/warp-notify.sh" "warp://cli-agent" "$BODY"

#!/bin/bash
# Hook script for Claude Code Notification event (idle_prompt and agent_needs_input)
# Sends a structured Warp notification when Claude has been idle or needs input

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/should-use-structured.sh"

# Legacy fallback for old Warp versions
if ! should_use_structured; then
    exit 0
fi

source "$SCRIPT_DIR/build-payload.sh"

# Read hook input from stdin
INPUT=$(cat)

# Extract notification-specific fields
NOTIF_TYPE=$(echo "$INPUT" | jq -r '.notification_type // "unknown"' 2>/dev/null)
# Harp: a permission or elicitation dialog (also one a subagent's tool raises) needs the user too.
case "$NOTIF_TYPE" in
    permission_prompt|elicitation_dialog) NOTIF_TYPE="permission_request" ;;
esac
MSG=$(echo "$INPUT" | jq -r '.message // "Input needed"' 2>/dev/null)
[ -z "$MSG" ] && MSG="Input needed"
if [ ${#MSG} -gt 120 ]; then
    MSG="${MSG:0:117}..."
fi

BODY=$(build_payload "$INPUT" "$NOTIF_TYPE" \
    --arg summary "$MSG")

"$SCRIPT_DIR/warp-notify.sh" "warp://cli-agent" "$BODY"

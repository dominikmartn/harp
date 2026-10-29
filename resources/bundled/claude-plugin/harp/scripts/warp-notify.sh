#!/bin/bash
# Warp notification utility using OSC escape sequences
# Usage: warp-notify.sh <title> <body>
#
# For structured Warp notifications, title should be "warp://cli-agent"
# and body should be a JSON string matching the cli-agent notification schema.
#
# Output behavior:
#   - On old Claude Code: writes OSC 777 directly to /dev/tty (no stdout)
#   - On new Claude Code (>= 2.1.141): prints {terminalSequence: ...} JSON to
#     stdout so the caller can pass it through as hook output

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/should-use-structured.sh"
source "$SCRIPT_DIR/emit-terminal-sequence.sh"

# Only emit notifications when we've confirmed the Warp build can render them.
if ! should_use_structured; then
    exit 0
fi

TITLE="${1:-Notification}"
BODY="${2:-}"

# OSC 777 format: \033]777;notify;<title>;<body>\007
SEQ=$(printf '\033]777;notify;%s;%s\007' "$TITLE" "$BODY")
emit_terminal_sequence "$SEQ"

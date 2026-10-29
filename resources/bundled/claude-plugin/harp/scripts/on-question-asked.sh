#!/bin/bash
# Harp: PreToolUse hook (matcher AskUserQuestion) that marks the row as "needs you" while Claude
# waits on a multiple-choice question. Warp's plugin only reports permission and idle prompts, so
# the row kept showing "working". The PostToolUse tool_complete flips it back once answered.
# Prints nothing a hook could read as a decision, so the tool is never blocked.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/should-use-structured.sh"

if ! should_use_structured; then
    exit 0
fi

source "$SCRIPT_DIR/build-payload.sh"

INPUT=$(cat)
SUMMARY=$(echo "$INPUT" | jq -r '(.tool_input.questions[0].question // "Has a question") | .[0:200]' 2>/dev/null)

BODY=$(build_payload "$INPUT" "question_asked" \
    --arg summary "${SUMMARY:-Has a question}")

"$SCRIPT_DIR/warp-notify.sh" "warp://cli-agent" "$BODY"

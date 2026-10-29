#!/bin/bash
# Hook script for Claude Code SessionStart event
# Shows welcome message, Warp detection status, and emits plugin version

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/should-use-structured.sh"

# Legacy fallback for old Warp versions
if ! should_use_structured; then
    exit 0
fi

if ! command -v jq &>/dev/null; then
    cat << 'EOF'
{
  "systemMessage": "Harp shows agent status with jq, which this Mac does not have. Install it with: brew install jq"
}
EOF
    exit 0
fi
source "$SCRIPT_DIR/build-payload.sh"

# Read hook input from stdin
INPUT=$(cat)

# Best-effort Claude Code version detection.
# Cache in $CLAUDE_ENV_FILE so subsequent hooks can skip the lookup, and
# export it now so the rest of this hook (warp-notify.sh below) can use it.
if [ -n "${CLAUDE_ENV_FILE:-}" ] && [ -z "${CLAUDE_CODE_VERSION:-}" ]; then
    CC_VERSION=$(claude --version 2>/dev/null | head -1 || true)
    if [ -n "$CC_VERSION" ]; then
        echo "export CLAUDE_CODE_VERSION=\"$CC_VERSION\"" >> "$CLAUDE_ENV_FILE"
        export CLAUDE_CODE_VERSION="$CC_VERSION"
    fi
fi

# Read plugin version from plugin.json
PLUGIN_VERSION=$(jq -r '.version // "unknown"' "$SCRIPT_DIR/../.claude-plugin/plugin.json" 2>/dev/null)

# Emit structured notification with plugin version so Warp can track it
BODY=$(build_payload "$INPUT" "session_start" \
    --arg plugin_version "$PLUGIN_VERSION")
"$SCRIPT_DIR/warp-notify.sh" "warp://cli-agent" "$BODY"

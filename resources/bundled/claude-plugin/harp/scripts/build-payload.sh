#!/bin/bash
# Builds a structured JSON notification payload for warp://cli-agent.
#
# Usage: source this file, then call build_payload with event-specific fields.
#
# Example:
#   source "$(dirname "${BASH_SOURCE[0]}")/build-payload.sh"
#   BODY=$(build_payload "$INPUT" "stop" \
#       --arg query "$QUERY" \
#       --arg response "$RESPONSE" \
#       --arg transcript_path "$TRANSCRIPT_PATH")
#
# The function extracts common fields (session_id, cwd, project) from the
# hook's stdin JSON (passed as $1), then merges any extra jq args you pass.

# The current protocol version this plugin knows how to produce.
PLUGIN_CURRENT_PROTOCOL_VERSION=1

# Negotiate the protocol version with Warp.
# Uses min(plugin_current, warp_declared), falling back to 1 if Warp doesn't advertise a version.
negotiate_protocol_version() {
    local warp_version="${WARP_CLI_AGENT_PROTOCOL_VERSION:-1}"
    if [ "$warp_version" -lt "$PLUGIN_CURRENT_PROTOCOL_VERSION" ] 2>/dev/null; then
        echo "$warp_version"
    else
        echo "$PLUGIN_CURRENT_PROTOCOL_VERSION"
    fi
}

build_payload() {
    local input="$1"
    local event="$2"
    shift 2

    local protocol_version
    protocol_version=$(negotiate_protocol_version)

    # Extract common fields from the hook input
    local session_id cwd project
    session_id=$(echo "$input" | jq -r '.session_id // empty' 2>/dev/null)
    cwd=$(echo "$input" | jq -r '.cwd // empty' 2>/dev/null)
    project=""
    if [ -n "$cwd" ]; then
        project=$(basename "$cwd")
    fi

    # Build the payload: common fields + any extra args passed by the caller.
    # Extra args should be jq flag pairs like: --arg key "value" or --argjson key '{"a":1}'
    jq -nc \
        --argjson v "$protocol_version" \
        --arg agent "claude" \
        --arg producer "harp" \
        --arg event "$event" \
        --arg session_id "$session_id" \
        --arg cwd "$cwd" \
        --arg project "$project" \
        "$@" \
        '{v:$v, agent:$agent, producer:$producer, event:$event, session_id:$session_id, cwd:$cwd, project:$project} + $ARGS.named'
}

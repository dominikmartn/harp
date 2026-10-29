#!/bin/bash
# Determines whether the current Warp build supports structured CLI agent notifications.
#
# Usage:
#   source "$SCRIPT_DIR/should-use-structured.sh"
#   if should_use_structured; then
#       # ... send structured notification
#   else
#       # ... legacy fallback or exit
#   fi
#
# Returns 0 (true) when structured notifications are safe to use, 1 (false) otherwise.

# Last known Warp release per channel that unconditionally set
# WARP_CLI_AGENT_PROTOCOL_VERSION without gating it behind the
# HOANotifications feature flag. These builds advertise protocol
# support but can't actually render structured notifications.
LAST_BROKEN_DEV=""
LAST_BROKEN_STABLE="v0.2026.03.25.08.24.stable_05"
LAST_BROKEN_PREVIEW="v0.2026.03.25.08.24.preview_05"

# Harp: macOS 15+ ships /usr/bin/jq, which a GUI-started shell may not have on PATH.
if ! command -v jq >/dev/null 2>&1 && [ -x /usr/bin/jq ]; then
    jq() { /usr/bin/jq "$@"; }
fi

should_use_structured() {
    # Harp: this copy of Warp's plugin is loaded only by Harp's terminals (CLAUDE_CODE_PLUGIN_DIRS)
    # and stays quiet anywhere else, such as a tmux pane reattached from another terminal.
    [ "${HARP_AGENT_STATUS:-}" = "1" ] || return 1

    # No protocol version advertised → Warp doesn't know about structured notifications.
    [ -z "${WARP_CLI_AGENT_PROTOCOL_VERSION:-}" ] && return 1

    # No client version available → can't verify this build has the fix.
    # (This catches the broken prod release before this was set, but after WARP_CLI_AGENT_PROTOCOL_VERSION was set without a flag check.)
    [ -z "${WARP_CLIENT_VERSION:-}" ] && return 1

    # Check whether this version is at or before the last broken release for its channel.
    local threshold=""
    case "$WARP_CLIENT_VERSION" in
        *dev*)     threshold="$LAST_BROKEN_DEV" ;;
        *stable*)  threshold="$LAST_BROKEN_STABLE" ;;
        *preview*) threshold="$LAST_BROKEN_PREVIEW" ;;
    esac

    # If we matched a channel and the version is <= the broken threshold, fall back.
    if [ -n "$threshold" ] && [[ ! "$WARP_CLIENT_VERSION" > "$threshold" ]]; then
        return 1
    fi

    return 0
}

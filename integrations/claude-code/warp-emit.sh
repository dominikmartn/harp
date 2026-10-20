# Sourced by warp-background-tasks.sh: sends one cli-agent event body to the app.
# Transport mirrors the official Warp Claude Code plugin (scripts/emit-terminal-sequence.sh):
# inside tmux the OSC 777 sequence goes through the pane's tty as a passthrough, otherwise it is
# returned as Claude Code's `terminalSequence` hook output.
# Inside tmux this needs `set -g allow-passthrough on`.
warp_emit() {
  local seq esc tty
  seq=$(printf '\033]777;notify;%s;%s\007' "warp://cli-agent" "$1")
  if [ -n "${TMUX:-}" ]; then
    esc=$'\033'
    tty=$(tmux display-message -p ${TMUX_PANE:+-t "$TMUX_PANE"} '#{pane_tty}' 2>/dev/null)
    if [ -n "$tty" ] && [ -w "$tty" ]; then
      seq="${seq//$esc/$esc$esc}"
      printf '%s' "${esc}Ptmux;${seq}${esc}\\" > "$tty" 2>/dev/null
      return 0
    fi
  fi
  jq -nc --arg seq "$seq" '{terminalSequence: $seq}'
}

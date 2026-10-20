# Claude Code background-tasks hook (optional)

The sidebar shows a Claude Code session as "working" while its turn runs. When a turn ends but the
session still has subagents or background shells running, Claude Code's own stop event cannot say
so reliably: the official Warp plugin's `background_task_count` lists every entry, including
monitors and tasks that already finished. This hook sends the real count of running work, so the
row keeps "working" until that work is done.

You do not need it. Without the hook the row shows "done" as soon as the turn ends, and the "done"
notification fires right away.

## Requirements

- The official Warp plugin for Claude Code, which sends the `stop`, `prompt_submit` and other
  session events this hook builds on.
- `bash`, `jq` and `perl` on your `PATH`.
- Inside tmux: `set -g allow-passthrough on`, so the escape sequence reaches the terminal.

## Install

```sh
mkdir -p ~/.claude/hooks
cp integrations/claude-code/warp-background-tasks.sh integrations/claude-code/warp-emit.sh ~/.claude/hooks/
chmod +x ~/.claude/hooks/warp-background-tasks.sh
```

Then add the hook to `~/.claude/settings.json` for both events:

```json
{
  "hooks": {
    "Stop": [
      { "hooks": [{ "type": "command", "command": "$HOME/.claude/hooks/warp-background-tasks.sh", "timeout": 5 }] }
    ],
    "SubagentStop": [
      { "hooks": [{ "type": "command", "command": "$HOME/.claude/hooks/warp-background-tasks.sh", "timeout": 5 }] }
    ]
  }
}
```

Merge these entries into any `Stop` and `SubagentStop` hooks you already have. Set
`WARP_HOOK_LOG=/path/to/file` in the hook's environment to log one line per call.

## Protocol

The hook writes one OSC 777 notification with the title `warp://cli-agent` and a JSON body:

```json
{"v": 1, "agent": "claude", "event": "background_tasks", "session_id": "…", "count": 2,
 "kinds": {"subagent": 1, "shell": 1}, "started_ms": 1760000000000}
```

- `count`: running background tasks, monitors excluded.
- `kinds`: running tasks per type. When every one is a `subagent`, the row says "agents".
- `started_ms`: when the hook started, in epoch milliseconds. Hooks of one session run side by
  side, so the app drops a report that started before the newest one it applied.

How the app behaves:

- Before the first report in an app run, a stop never waits for one (no hook installed).
- After that, a stop that arrives before its turn's report waits up to 4 seconds for it, then
  counts as done.
- A count above zero keeps the row "working"; the "done" notification fires once it drops to zero.

The parser is `app/src/terminal/cli_agent_sessions/event/v1.rs`, the state machine
`app/src/terminal/cli_agent_sessions/mod.rs`, and the replay tests (built from real stop sequences)
are in `mod_tests.rs` next to it.

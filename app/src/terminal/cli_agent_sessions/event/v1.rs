use std::collections::HashMap;

use serde::Deserialize;
use warp_core::cli_agent_protocol::CLIAgentNotification;

use super::{CLIAgentEvent, CLIAgentEventPayload, CLIAgentEventSource, CLIAgentEventType};
use crate::terminal::CLIAgent;

/// Resolves a CLI agent from the `"agent"` string in a CLI agent event.
/// Returns `None` if the string doesn't match any known agent.
fn resolve_agent(agent: &str) -> Option<CLIAgent> {
    enum_iterator::all::<CLIAgent>().find(|candidate| candidate.command_prefixes().contains(&agent))
}

/// Fields only the `background_tasks` event carries; they are not part of the shared wire struct
/// because the event comes from the user's own hook, not from the Warp plugin.
#[derive(Deserialize)]
struct BackgroundTasksFields {
    count: Option<u32>,
    kinds: Option<HashMap<String, u32>>,
    started_ms: Option<u64>,
}

fn parse_background_tasks(body: &str) -> Option<CLIAgentEventType> {
    let fields: BackgroundTasksFields = serde_json::from_str(body).ok()?;
    let kinds = fields.kinds.unwrap_or_default();
    Some(CLIAgentEventType::BackgroundTasks {
        count: fields.count?,
        only_subagents: !kinds.is_empty() && kinds.keys().all(|kind| kind == "subagent"),
        shells: kinds.get("shell").copied().unwrap_or_default(),
        started_ms: fields.started_ms,
    })
}

pub(super) fn parse(body: &str) -> Option<CLIAgentEvent> {
    let raw: CLIAgentNotification = serde_json::from_str(body).ok()?;

    let event = match raw.event.as_str() {
        "session_start" => CLIAgentEventType::SessionStart,
        // WarpOss: Antigravity sends this at the start of every task, i.e. a turn.
        "task_started" => CLIAgentEventType::PromptSubmit,
        "prompt_submit" => CLIAgentEventType::PromptSubmit,
        "tool_complete" => CLIAgentEventType::ToolComplete,
        "stop" | "task_complete" => CLIAgentEventType::Stop,
        "stop_failure" => CLIAgentEventType::StopFailure,
        "permission_request" => CLIAgentEventType::PermissionRequest,
        "permission_replied" => CLIAgentEventType::PermissionReplied,
        "question_asked" => CLIAgentEventType::QuestionAsked,
        "agent_needs_input" | "prompt_required" => CLIAgentEventType::NeedsInput,
        "idle_prompt" => CLIAgentEventType::IdlePrompt,
        "background_tasks" => parse_background_tasks(body)?,
        other => CLIAgentEventType::Unknown(other.to_string()),
    };

    let tool_input_preview = raw.tool_input.as_ref().and_then(|val| {
        val.get("command")
            .or_else(|| val.get("file_path"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    let agent = raw
        .agent
        .as_deref()
        .and_then(resolve_agent)
        .unwrap_or(CLIAgent::Unknown);

    // WarpOss: only running background tasks keep a stopped turn "working". A session cron is a
    // wakeup scheduled for later; until it fires the chat sits idle, and counting it kept tabs
    // with a `/loop` working for hours.
    let pending_background_work_count = match (raw.background_task_count, raw.session_cron_count) {
        (None, None) => None,
        (tasks, _crons) => Some(tasks.unwrap_or(0)),
    };

    Some(CLIAgentEvent {
        v: raw.v.unwrap_or(1),
        agent,
        event,
        session_id: raw.session_id,
        cwd: raw.cwd,
        project: raw.project,
        payload: CLIAgentEventPayload {
            query: raw.query,
            response: raw.response,
            transcript_path: raw.transcript_path,
            summary: raw.summary,
            tool_name: raw.tool_name,
            tool_input_preview,
            plugin_version: raw.plugin_version,
            error_type: raw.error_type,
            pending_background_work_count,
        },
        source: CLIAgentEventSource::RichPlugin,
    })
}

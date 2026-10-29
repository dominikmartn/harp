mod v1;

use serde::Deserialize;
pub use warp_core::cli_agent_protocol::CLI_AGENT_NOTIFICATION_SENTINEL;
use warp_errors::report_error;

use crate::terminal::CLIAgent;

#[cfg_attr(not(feature = "local_tty"), allow(dead_code))]
type EventParser = fn(&str) -> Option<CLIAgentEvent>;

/// The event type encoded in the `"event"` field of the JSON body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CLIAgentEventType {
    SessionStart,
    PromptSubmit,
    ToolComplete,
    Stop,
    StopFailure,
    PermissionRequest,
    PermissionReplied,
    QuestionAsked,
    NeedsInput,
    IdlePrompt,
    /// Background work (subagents, shells, monitors) still in flight after a turn, reported by
    /// the user's own Stop/SubagentStop hook from Claude Code's `background_tasks` list.
    BackgroundTasks {
        count: u32,
        only_subagents: bool,
        /// How many of `count` are background shells, from the hook's `kinds`. A report without
        /// `kinds` (an older hook) counts none, so all of its work keeps the row working.
        shells: u32,
        /// When the hook started (Unix ms). Hooks run side by side and a slow one can report
        /// after a newer one, so a report older than the last one applied is stale.
        started_ms: Option<u64>,
    },
    Unknown(String),
}

/// How a CLI agent event reached Warp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CLIAgentEventSource {
    /// Structured OSC 777 notification from a rich plugin.
    RichPlugin,
    /// Native Codex OSC 9 fallback notification.
    CodexOsc9Fallback,
    /// Harp: read from the terminal title the agent sets itself (Claude Code's ◐/✳, Codex's
    /// braille spinner), for an agent that runs without a status plugin.
    TerminalTitle,
}

/// Harp: what an agent's own terminal title says about its turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleState {
    Working,
    Idle,
    NeedsYou,
}

/// Harp: reads the state an agent puts at the front of its terminal title. Claude Code turns a
/// half-disc (◐ ◑ ◒ ◓) while it works and shows ✳ when idle; while it starts up it already spins
/// with its default title "Claude Code", which is no turn yet. Codex spins a braille character in
/// front of the task while it works, drops it when done, and blinks `[ ! ] Action Required` /
/// `[ . ] Action Required` in front while it waits for an approval or an answer (Codex 0.162).
/// `None` when the title says nothing.
pub fn title_state(agent: CLIAgent, title: &str) -> Option<TitleState> {
    let mut chars = title.chars();
    let first = chars.next()?;
    let rest = chars.as_str().trim();
    match agent {
        CLIAgent::Claude => match first {
            '◐' | '◑' | '◒' | '◓' if rest != "Claude Code" => Some(TitleState::Working),
            '✳' => Some(TitleState::Idle),
            _ => None,
        },
        CLIAgent::Codex if codex_action_required_prefix(title).is_some() => {
            Some(TitleState::NeedsYou)
        }
        CLIAgent::Codex if ('\u{2801}'..='\u{28FF}').contains(&first) => Some(TitleState::Working),
        CLIAgent::Codex if !title.trim().is_empty() => Some(TitleState::Idle),
        _ => None,
    }
}

/// Harp: the `[ ! ] Action Required | ` (or blinking `[ . ] …`) Codex puts in front of its title
/// while it waits on the user, when `title` starts with it.
pub fn codex_action_required_prefix(title: &str) -> Option<&'static str> {
    ["[ ! ] Action Required | ", "[ . ] Action Required | "]
        .into_iter()
        .find(|prefix| title.starts_with(prefix))
}

/// Event-specific fields that vary by event type.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct CLIAgentEventPayload {
    pub query: Option<String>,
    pub response: Option<String>,
    pub transcript_path: Option<String>,
    pub summary: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input_preview: Option<String>,
    pub plugin_version: Option<String>,
    /// On Claude Code, this comes from the `StopFailure` hook (e.g. `"rate_limit"`).
    /// Not implemented for Codex.
    pub error_type: Option<String>,
    /// Background tasks and scheduled wakeups still pending when a `Stop` fired. Claude Code
    /// wakes the agent again when any of them completes or fires, so a stop with pending work is
    /// a pause, not the end of the session. `None` when the plugin predates this field.
    pub pending_background_work_count: Option<u32>,
    /// Harp: sent by Harp's bundled Claude Code plugin (`"producer":"harp"`). Once a session has
    /// one, events from any other plugin in the same pane (Warp's own, installed by the user) are
    /// duplicates and dropped.
    pub from_harp_plugin: bool,
}

impl CLIAgentEventPayload {
    /// Whether the agent reported work that will wake it again without user input.
    pub fn has_pending_background_work(&self) -> bool {
        self.pending_background_work_count
            .is_some_and(|count| count > 0)
    }
}

/// A parsed event from a CLI agent plugin.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CLIAgentEvent {
    pub v: u32,
    pub agent: CLIAgent,
    pub event: CLIAgentEventType,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub payload: CLIAgentEventPayload,
    pub source: CLIAgentEventSource,
}

/// Version-specific parsers, indexed by (version - 1).
/// Adding a new version means appending a parser here.
#[cfg_attr(not(feature = "local_tty"), allow(dead_code))]
const VERSIONED_PARSERS: &[EventParser] = &[v1::parse];

/// Attempts to parse an OSC 777 `PluggableNotification` into a typed `CLIAgentEvent`.
/// Dispatches to the correct version-specific parser based on the `"v"` field. Returns `None`
/// if the title doesn't match the sentinel, the body isn't valid JSON, or the version is unsupported.
pub fn parse_event(title: Option<&str>, body: &str) -> Option<CLIAgentEvent> {
    if title? != CLI_AGENT_NOTIFICATION_SENTINEL {
        return None;
    }

    let version_probe: VersionProbe = serde_json::from_str(body).ok()?;
    let version = version_probe.v.unwrap_or(1);

    let index = (version as usize).checked_sub(1)?;
    match VERSIONED_PARSERS.get(index) {
        Some(parser) => parser(body),
        None => {
            report_error!(
                "Received CLI agent event with unsupported schema version. The CLI agent plugin or Warp may need to be updated.",
                extra: { "version" => %version }
            );
            None
        }
    }
}

#[derive(Deserialize)]
struct VersionProbe {
    v: Option<u32>,
}

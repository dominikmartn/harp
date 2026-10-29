pub mod event;
pub mod listener;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod plugin_manager;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Local};

use event::{CLIAgentEvent, CLIAgentEventSource, CLIAgentEventType};
use warp_core::execution_mode::AppExecutionMode;
use warpui::r#async::SpawnedFutureHandle;
use warpui::{Entity, EntityId, ModelContext, ModelHandle, SingletonEntity};

use self::listener::CLIAgentSessionListener;
use super::CLIAgent;
use crate::ai::blocklist::InputConfig;

/// How long to wait, after observing a synthesized Ctrl-C write to a working
/// CLI agent session's PTY, for further plugin activity before concluding the
/// interrupt silently cancelled the session. See `observe_ctrl_c_write`.
pub const CTRL_C_CANCEL_WINDOW: Duration = Duration::from_secs(2);

/// How long a finished session stays "working" after a tool completes without a new prompt.
/// Background tasks and subagents keep producing tool events while they run, so silence this long
/// means the event was a straggler from the finished turn.
const PROVISIONAL_WORK_WINDOW: Duration = Duration::from_secs(60);
/// WarpOss: how long a quiet stop waits for the background-tasks hook's count of its turn. Both
/// are Stop hooks; in 96 real stops they arrived at most 2 s apart.
const BACKGROUND_REPORT_WAIT: Duration = Duration::from_secs(4);

const NEEDS_INPUT_STATUS_MESSAGE: &str = "Agent requires user input";

/// What caused a session to become [`CLIAgentSessionStatus::Blocked`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedSource {
    PermissionRequest,
    QuestionAsked,
    NeedsInput,
}

/// Status of a tracked CLI agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CLIAgentSessionStatus {
    InProgress,
    Success,
    Failed {
        error_type: Option<String>,
        message: Option<String>,
    },
    Blocked {
        message: Option<String>,
        source: BlockedSource,
    },
    /// The user interrupted the session with Ctrl-C and no further plugin
    /// activity was observed within the grace window (see
    /// `observe_ctrl_c_write`). Not terminal: a later `prompt_submit`
    /// returns the session to `InProgress` like any other resumed turn.
    Cancelled,
}

impl CLIAgentSessionStatus {
    pub fn to_conversation_status(&self) -> crate::ai::agent::conversation::ConversationStatus {
        use crate::ai::agent::conversation::ConversationStatus;
        match self {
            CLIAgentSessionStatus::InProgress => ConversationStatus::InProgress,
            CLIAgentSessionStatus::Success => ConversationStatus::Success,
            CLIAgentSessionStatus::Failed { .. } => ConversationStatus::Error,
            CLIAgentSessionStatus::Blocked { message, .. } => ConversationStatus::Blocked {
                blocked_action: message.clone().unwrap_or_default(),
            },
            CLIAgentSessionStatus::Cancelled => ConversationStatus::Cancelled,
        }
    }

    /// WarpOss: the status the sidebar shows. A finished turn with background work still
    /// running reads as working.
    pub(crate) fn displayed(&self, has_background_work: bool) -> CLIAgentSessionStatus {
        match self {
            CLIAgentSessionStatus::Success if has_background_work => {
                CLIAgentSessionStatus::InProgress
            }
            status => status.clone(),
        }
    }
}

/// WarpOss: whether a status change is news worth a desktop notification (and its sound). Only
/// a change the sidebar shows is: the row turns "needs you", or turns finished from working.
/// Repeats stay silent (the plugin and the permission hook both report one dialog), and so does a
/// turn that ends while background work keeps the row working. A turn that ends after a Ctrl-C
/// was the user's own doing.
pub(crate) fn status_change_is_announced(
    previous: &CLIAgentSessionStatus,
    previous_has_background_work: bool,
    new: &CLIAgentSessionStatus,
    new_has_background_work: bool,
) -> bool {
    let before = previous.displayed(previous_has_background_work);
    match new.displayed(new_has_background_work) {
        CLIAgentSessionStatus::Blocked { .. } => {
            !matches!(before, CLIAgentSessionStatus::Blocked { .. })
        }
        CLIAgentSessionStatus::Success | CLIAgentSessionStatus::Failed { .. } => !matches!(
            before,
            CLIAgentSessionStatus::Success
                | CLIAgentSessionStatus::Failed { .. }
                | CLIAgentSessionStatus::Cancelled
        ),
        CLIAgentSessionStatus::InProgress | CLIAgentSessionStatus::Cancelled => false,
    }
}

/// How far a background-tasks report may start before the last one applied and still be dropped
/// as stale (see `is_stale_background_report`).
const STALE_REPORT_WINDOW_MS: u64 = 30_000;

/// Background work a session still has in flight after its turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackgroundTasks {
    pub count: u32,
    /// Every running task is a subagent, so the UI can say "agents" instead of "tasks".
    pub only_subagents: bool,
    /// How many of `count` are shells (builds, tests, servers the agent started).
    pub shells: u32,
}

/// Rich context accumulated from CLI agent session events.
#[derive(Debug, Clone, Default)]
pub struct CLIAgentSessionContext {
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input_preview: Option<String>,
    pub summary: Option<String>,
    pub query: Option<String>,
    pub response: Option<String>,
    /// Latest report of background work; `None` until the first report, so a missing hook reads
    /// as unknown rather than as "nothing running".
    pub background_tasks: Option<BackgroundTasks>,
    /// WarpOss: the background-tasks hook reported during the current turn, so its count
    /// belongs to the turn that stops next. A new turn and a report that settles a waiting stop
    /// leave it unset.
    pub background_reported_this_turn: bool,
    /// WarpOss: the last stop came before the hook's count for its turn while the plugin listed
    /// background entries. Holds what the row showed before that stop: the turn's "done" is
    /// announced by the same rule once the hook says nothing runs, or when no report comes.
    pub awaiting_background_report: Option<CLIAgentSessionStatus>,
    /// WarpOss: start time of the newest background-tasks report applied.
    pub last_background_report_ms: Option<u64>,
    /// WarpOss: the optional background-tasks hook (integrations/claude-code) has reported in
    /// this app run. Until it has, a stop never waits for a report that will not come, so a
    /// setup without the hook gets its "done" right away.
    pub background_hook_installed: bool,
    /// WarpOss: the tools the open permission dialogs ask about, oldest first, each with its
    /// target (command or file) when the event names one. Only that tool on that target finishing
    /// answers a dialog: background agents keep running tools, the same ones too, meanwhile.
    pub pending_permission_tools: Vec<(String, Option<String>)>,
    /// WarpOss: the main agent's turn ended while a background agent's dialog kept the row on
    /// "needs you", so answering that dialog leaves a finished turn behind.
    pub turn_ended_while_blocked: bool,
    /// Harp: something beyond the session's start has been reported (a prompt, a tool, a stop, a
    /// dialog). Until then the agent has done nothing, and the row shows an empty ring rather than
    /// the tick of a finished turn.
    pub had_activity: bool,
    /// Harp: the agent runs without a status plugin, and its status comes from the terminal title
    /// it sets (see `CLIAgentEventSource::TerminalTitle`). A plugin event takes over for good.
    pub status_from_title: bool,
}

/// State of the rich input editor for composing a prompt to send to a CLI agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CLIAgentInputState {
    /// The rich input editor is not open.
    Closed,
    /// The rich input editor is open.
    Open {
        /// How this session was opened (for telemetry).
        entrypoint: CLIAgentInputEntrypoint,
        /// The input config that was active before opening rich input.
        previous_input_config: InputConfig,
        /// Whether the previous lock state was established while the input buffer was empty.
        previous_was_lock_set_with_empty_buffer: bool,
    },
}

/// Why the CLI agent rich input was closed (for telemetry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CLIAgentRichInputCloseReason {
    /// User explicitly closed (Escape, Ctrl-G, footer button).
    Manual,
    /// Auto-closed due to agent status change (e.g. Blocked).
    AutoToggle,
    /// Auto-dismissed after submitting a prompt.
    Submit,
    /// Closed for another reason (chip removed, session ended, shared session sync).
    Other,
}

/// How a [`CLIAgentInputState`] was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CLIAgentInputEntrypoint {
    /// User pressed Ctrl-G while a CLI agent was active.
    CtrlG,
    /// User clicked the rich input button in the CLI agent footer.
    FooterButton,
    /// Automatically opened when the CLI agent resumed work (left a blocked state)
    /// and the auto-show setting is enabled.
    AutoShow,
    /// Rich input was opened to mirror a shared-session participant's state.
    SharedSessionSync,
}

impl CLIAgentSessionContext {
    /// Background work that keeps a finished turn "working", when the latest report says any
    /// runs. WarpOss: shells alone do not (variant A): a turn that leaves only a build or a
    /// server running reads as done, with `has_background_shells` as a mark on the row.
    pub(crate) fn running_background_tasks(&self) -> Option<BackgroundTasks> {
        self.background_tasks
            .filter(|tasks| tasks.count > tasks.shells)
    }

    /// WarpOss: the latest report lists background shells still running.
    pub(crate) fn has_background_shells(&self) -> bool {
        self.background_tasks.is_some_and(|tasks| tasks.shells > 0)
    }

    pub(crate) fn display_title(&self) -> Option<String> {
        self.latest_user_prompt().or_else(|| self.title_like_text())
    }

    pub(crate) fn latest_user_prompt(&self) -> Option<String> {
        self.query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_owned)
    }

    /// Returns summary text suitable as a fallback title when no user prompt is available.
    pub(crate) fn title_like_text(&self) -> Option<String> {
        self.summary
            .as_deref()
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
            .map(str::to_owned)
    }
}

/// A tracked CLI agent session.
#[derive(Debug, Clone)]
pub struct CLIAgentSession {
    pub agent: CLIAgent,
    pub status: CLIAgentSessionStatus,
    pub session_context: CLIAgentSessionContext,
    /// Rich input editor state.
    pub input_state: CLIAgentInputState,
    /// Whether status-driven auto-toggle is enabled for this session.
    pub should_auto_toggle_input: bool,
    /// Event listener for plugin-backed sessions or Codex OSC9 fallback.
    /// `None` for non-Codex sessions created by command detection alone.
    /// Dropping this handle cleans up the listener's PTY event subscription.
    pub listener: Option<ModelHandle<CLIAgentSessionListener>>,
    /// The plugin version reported by structured plugin events.
    /// `None` if the plugin predates version reporting or Codex is using OSC9 fallback.
    pub plugin_version: Option<String>,
    /// `None` when the session is local.
    /// `Some("user@hostname")` when running over SSH (warpified or legacy).
    /// Used as a key for per-host plugin install failure tracking.
    pub remote_host: Option<String>,
    /// Draft text saved from the rich input composer when it was closed.
    /// Restored into the editor when the composer is reopened.
    pub draft_text: Option<String>,
    /// When the session was detected via a custom toolbar command pattern,
    /// the first word of the command (the binary/alias the user typed).
    /// Used to customize plugin instructions and force manual install mode.
    pub custom_command_prefix: Option<String>,
    /// Set once the session has received any structured OSC 777 (rich)
    /// notification. Codex's OSC 9 fallback never sets it, so this is the
    /// single source of truth for whether the session is plugin-backed.
    pub received_rich_notification: bool,
}

impl CLIAgentSession {
    pub fn is_remote(&self) -> bool {
        self.remote_host.is_some()
    }

    /// Whether the session surfaces trustworthy fine-grained status
    /// (in-progress / blocked / success). True only after receiving a rich OSC
    /// 777 notification. Codex's OSC 9 fallback emits only opaque `Stop`
    /// notifications and never sets `received_rich_notification`, so it does
    /// not qualify. Synthetic listener registration also does not qualify until
    /// an actual rich notification arrives.
    pub fn supports_rich_status(&self) -> bool {
        self.received_rich_notification
    }

    /// Harp: a status plugin reports for this session. Codex's OSC 9 fallback registers like a
    /// plugin but carries no plugin version, and only ever reports a turn's end.
    pub fn has_status_plugin(&self) -> bool {
        self.received_rich_notification
            && !(self.agent == CLIAgent::Codex && self.plugin_version.is_none())
    }

    /// Harp: the sidebar has a status for this session, from a plugin or from the title.
    pub fn shows_status(&self) -> bool {
        self.received_rich_notification || self.session_context.status_from_title
    }

    /// Whether the session is blocked on a `NeedsInput` notification, so nothing should be
    /// typed into its terminal.
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    pub fn is_blocked_on_needs_input(&self) -> bool {
        matches!(
            self.status,
            CLIAgentSessionStatus::Blocked {
                source: BlockedSource::NeedsInput,
                ..
            }
        )
    }

    /// Clears state populated by `PermissionRequest`. Called whenever the
    /// session leaves the permission flow (the user replied, a blocking tool
    /// completed, a new prompt is submitted, or the session ends successfully)
    /// so the permission summary doesn't leak into later UI surfaces — most
    /// visibly the tab title, which can fall back to `summary` when `query`
    /// is unset.
    fn clear_permission_scoped_state(&mut self) {
        self.session_context.summary = None;
        self.session_context.tool_name = None;
        self.session_context.tool_input_preview = None;
        self.session_context.pending_permission_tools.clear();
        self.session_context.turn_ended_while_blocked = false;
    }

    /// WarpOss: a background agent's permission dialog is open. It outlives the main agent's
    /// turns: prompts and stops of the main agent leave the row on "needs you" until the tool
    /// it asks about finishes or no background work is left.
    fn has_background_permission_dialog(&self) -> bool {
        matches!(
            self.status,
            CLIAgentSessionStatus::Blocked {
                source: BlockedSource::PermissionRequest,
                ..
            }
        ) && !self.session_context.pending_permission_tools.is_empty()
            && self.session_context.running_background_tasks().is_some()
    }

    /// Applies an event to this session, updating context and status.
    /// Returns the new status if it changed, or `None` if the event was irrelevant.
    /// WarpOss: a background-tasks report from a hook that started before the last one applied.
    /// It changes nothing, not even a waiting stop: the newer report already spoke for the turn.
    /// Claude Code kills the hook after its 5 s timeout, so a real lag stays well inside the
    /// window; a bigger gap means the clock was set back, and that report counts.
    fn is_stale_background_report(&self, event: &CLIAgentEvent) -> bool {
        match (&event.event, self.session_context.last_background_report_ms) {
            (
                CLIAgentEventType::BackgroundTasks {
                    started_ms: Some(started),
                    ..
                },
                Some(last),
            ) => *started < last && last - *started <= STALE_REPORT_WINDOW_MS,
            _ => false,
        }
    }

    fn apply_event(&mut self, event: &CLIAgentEvent) -> Option<CLIAgentSessionStatus> {
        self.session_context.cwd = event.cwd.clone().or(self.session_context.cwd.take());
        self.session_context.project = event
            .project
            .clone()
            .or(self.session_context.project.take());
        self.session_context.session_id = event
            .session_id
            .clone()
            .or(self.session_context.session_id.take());
        if event.event != CLIAgentEventType::SessionStart {
            self.session_context.had_activity = true;
        }

        let new_status = match &event.event {
            CLIAgentEventType::PromptSubmit => {
                self.session_context.background_reported_this_turn = false;
                self.session_context.query = event.payload.query.clone();
                self.session_context.response = None;
                if self.has_background_permission_dialog() {
                    self.session_context.turn_ended_while_blocked = false;
                    return None;
                }
                self.clear_permission_scoped_state();
                CLIAgentSessionStatus::InProgress
            }
            CLIAgentEventType::ToolComplete => {
                match self.status {
                    // A straggler from before a Ctrl-C must not resurrect a cancelled session.
                    CLIAgentSessionStatus::InProgress | CLIAgentSessionStatus::Cancelled => {
                        return None;
                    }
                    CLIAgentSessionStatus::Blocked { .. } => {
                        // WarpOss: only the tool a dialog asks about answers it. A dialog that named
                        // no tool ends with any tool, as before.
                        let pending = &mut self.session_context.pending_permission_tools;
                        if !pending.is_empty() {
                            // Without a target on either side (an older plugin) the name decides.
                            let target = event.payload.tool_input_preview.as_ref();
                            let Some(index) = event.payload.tool_name.as_ref().and_then(|tool| {
                                pending.iter().position(|(name, asked)| {
                                    name == tool
                                        && (asked.is_none() || target.is_none() || asked.as_ref() == target)
                                })
                            }) else {
                                return None;
                            };
                            pending.remove(index);
                            if !pending.is_empty() {
                                return None;
                            }
                        }
                        let turn_ended = self.session_context.turn_ended_while_blocked;
                        self.clear_permission_scoped_state();
                        if turn_ended {
                            CLIAgentSessionStatus::Success
                        } else {
                            CLIAgentSessionStatus::InProgress
                        }
                    }
                    // Turns woken by background tasks or subagents send no prompt_submit, so a tool
                    // finishing after the last stop is the only sign the agent is working again.
                    CLIAgentSessionStatus::Success | CLIAgentSessionStatus::Failed { .. } => {
                        self.session_context.background_reported_this_turn = false;
                        CLIAgentSessionStatus::InProgress
                    }
                }
            }
            CLIAgentEventType::Stop => {
                self.session_context.query = event.payload.query.clone();
                self.session_context.response = event.payload.response.clone();
                if self.has_background_permission_dialog() {
                    self.session_context.background_reported_this_turn = false;
                    self.session_context.turn_ended_while_blocked = true;
                    return None;
                }
                self.clear_permission_scoped_state();
                // WarpOss: only the background-tasks hook's count says whether work still runs.
                // The plugin's `background_task_count` lists every entry, monitors and finished
                // tasks included (72 of 96 recorded real stops had a higher plugin count),
                // so it never keeps a row working. A hook count from before the last stop is
                // an older turn's and reads as unknown, i.e. done.
                // What the row showed before this stop, for announcing a turn that waits.
                let shown_before = self
                    .session_context
                    .awaiting_background_report
                    .take()
                    .unwrap_or_else(|| {
                        self.status
                            .displayed(self.session_context.running_background_tasks().is_some())
                    });
                if !self.session_context.background_reported_this_turn {
                    self.session_context.background_tasks = None;
                }
                self.session_context.background_reported_this_turn = false;
                // The hook's count for this turn usually arrives first; when it has not yet and
                // the plugin lists entries, the "done" waits for it (see `update_from_event`).
                if self.session_context.background_hook_installed
                    && self.session_context.background_tasks.is_none()
                    && event.payload.has_pending_background_work()
                {
                    self.session_context.awaiting_background_report = Some(shown_before);
                }
                // The agent will wake itself again once that work finishes, so this turn
                // boundary is a pause rather than the end of the session. WarpOss: the turn
                // itself is over, so the status says finished without announcing it (no
                // notification); views show it as working while the hook's count is above
                // zero, and as done once that work reports finished.
                if self.session_context.running_background_tasks().is_some()
                    || self.session_context.awaiting_background_report.is_some()
                {
                    self.status = CLIAgentSessionStatus::Success;
                    return None;
                }
                CLIAgentSessionStatus::Success
            }
            CLIAgentEventType::StopFailure => {
                self.session_context.query = event.payload.query.clone();
                self.session_context.response = event.payload.response.clone();
                self.clear_permission_scoped_state();
                CLIAgentSessionStatus::Failed {
                    error_type: event.payload.error_type.clone(),
                    message: event.payload.response.clone(),
                }
            }
            CLIAgentEventType::PermissionRequest => {
                // Claude Code reminds of a dialog that waits with a second request naming no
                // tool; the first one already said what it asks about.
                match &event.payload.tool_name {
                    Some(tool) => self.session_context.pending_permission_tools.push((
                        tool.clone(),
                        event.payload.tool_input_preview.clone(),
                    )),
                    None if matches!(
                        self.status,
                        CLIAgentSessionStatus::Blocked {
                            source: BlockedSource::PermissionRequest,
                            ..
                        }
                    ) =>
                    {
                        return None;
                    }
                    None => {}
                }
                self.session_context.summary = event.payload.summary.clone();
                self.session_context.tool_name = event.payload.tool_name.clone();
                self.session_context.tool_input_preview = event.payload.tool_input_preview.clone();
                CLIAgentSessionStatus::Blocked {
                    message: event.payload.summary.clone(),
                    source: BlockedSource::PermissionRequest,
                }
            }
            CLIAgentEventType::QuestionAsked => CLIAgentSessionStatus::Blocked {
                message: event
                    .payload
                    .summary
                    .clone()
                    .or_else(|| Some("Waiting for your answer".to_owned())),
                source: BlockedSource::QuestionAsked,
            },
            CLIAgentEventType::NeedsInput => CLIAgentSessionStatus::Blocked {
                message: event
                    .payload
                    .summary
                    .clone()
                    .or_else(|| Some(NEEDS_INPUT_STATUS_MESSAGE.to_owned())),
                source: BlockedSource::NeedsInput,
            },
            CLIAgentEventType::PermissionReplied => {
                if !matches!(self.status, CLIAgentSessionStatus::Blocked { .. }) {
                    return None;
                }
                self.clear_permission_scoped_state();
                CLIAgentSessionStatus::InProgress
            }
            // IdlePrompt means the agent is sitting at its prompt waiting for input.
            // This should not affect status — otherwise it would override Success after a Stop event.
            CLIAgentEventType::IdlePrompt => return None,
            CLIAgentEventType::SessionStart => {
                self.plugin_version = event.payload.plugin_version.clone();
                return None;
            }
            // Leaves the foreground status alone: views combine a finished turn with running
            // background work into "working". Only a background agent's dialog that outlived the
            // turn ends here, once no background work is left.
            CLIAgentEventType::BackgroundTasks {
                count,
                only_subagents,
                shells,
                started_ms,
            } => {
                self.session_context.background_hook_installed = true;
                if started_ms.is_some() {
                    self.session_context.last_background_report_ms = *started_ms;
                }
                self.session_context.background_tasks = Some(BackgroundTasks {
                    count: *count,
                    only_subagents: *only_subagents,
                    shells: (*shells).min(*count),
                });
                // A report that settles a waiting stop belongs to that stop's turn.
                self.session_context.background_reported_this_turn =
                    self.session_context.awaiting_background_report.is_none();
                // WarpOss: with no background work left, no background agent's dialog can still
                // be open (one that was denied finishes no tool). A turn that ended meanwhile is
                // done; one still running clears the dialog with its next tool or stop.
                if !(self.session_context.turn_ended_while_blocked
                    && self.session_context.running_background_tasks().is_none())
                {
                    return None;
                }
                self.clear_permission_scoped_state();
                CLIAgentSessionStatus::Success
            }
            CLIAgentEventType::Unknown(_) => return None,
        };

        self.status = new_status.clone();
        Some(new_status)
    }
}

/// Events emitted by `CLIAgentSessionsModel` for subscribers (e.g., `AgentNotificationsModel`).
#[allow(dead_code)] // `agent` fields on Started/InputSessionChanged/Ended are used for logging and future subscribers.
#[derive(Debug, Clone)]
pub enum CLIAgentSessionsModelEvent {
    Started {
        terminal_view_id: EntityId,
        agent: CLIAgent,
    },
    StatusChanged {
        terminal_view_id: EntityId,
        agent: CLIAgent,
        status: CLIAgentSessionStatus,
        is_prompt_submit: bool,
        session_context: Box<CLIAgentSessionContext>,
        /// WarpOss: whether this change may send a desktop notification, see
        /// [`status_change_is_announced`].
        announce: bool,
    },
    InputSessionChanged {
        terminal_view_id: EntityId,
        agent: CLIAgent,
        /// The input state BEFORE this change. When transitioning from
        /// `Open` → `Closed`, contains the saved input config to restore.
        previous_input_state: CLIAgentInputState,
        /// The input state AFTER this change.
        new_input_state: CLIAgentInputState,
    },
    Ended {
        terminal_view_id: EntityId,
        agent: CLIAgent,
    },
    /// The agent session has been updated. Subscribers may use this as a trigger for best-effort
    /// saving of state derived from the agent's session.
    SessionUpdated {
        terminal_view_id: EntityId,
        agent: CLIAgent,
    },
}

impl CLIAgentSessionsModelEvent {
    pub fn terminal_view_id(&self) -> EntityId {
        match self {
            CLIAgentSessionsModelEvent::Started {
                terminal_view_id, ..
            }
            | CLIAgentSessionsModelEvent::StatusChanged {
                terminal_view_id, ..
            }
            | CLIAgentSessionsModelEvent::InputSessionChanged {
                terminal_view_id, ..
            }
            | CLIAgentSessionsModelEvent::Ended {
                terminal_view_id, ..
            }
            | CLIAgentSessionsModelEvent::SessionUpdated {
                terminal_view_id, ..
            } => *terminal_view_id,
        }
    }
}

/// A finished session that a tool event flipped back to `InProgress` without a `prompt_submit`.
struct ProvisionalWork {
    restore: CLIAgentSessionStatus,
    restore_changed_at: Option<DateTime<Local>>,
    /// Same stale-callback guard as `CtrlCCancelState::armed_token`.
    token: u64,
    handle: SpawnedFutureHandle,
}

/// Per-session state for a Ctrl-C-initiated pending cancellation. Kept
/// separate from `CLIAgentSession` because it is synthesized entirely
/// client-side (see `observe_ctrl_c_write`) rather than reported by the
/// plugin protocol.
#[derive(Default)]
struct CtrlCCancelState {
    /// Whether a `prompt_submit` has been seen for this session. Guards
    /// against arming on the optimistic `InProgress` status set when a
    /// session is first registered, before any turn has actually started.
    has_seen_prompt_submit: bool,
    /// Abort handle for the in-flight grace-window timer, if armed.
    pending_cancel: Option<SpawnedFutureHandle>,
    /// Identifies the window `pending_cancel` belongs to. `SpawnedFutureHandle::abort`
    /// only takes effect the next time the future is polled, so a timer that has
    /// already completed (and queued its resolve callback) can still run after
    /// `abort()` is called. The callback captures this token and only acts if it
    /// still matches when it fires, so a stale callback racing a disarming event
    /// is a no-op instead of overwriting that event's status with `Cancelled`.
    armed_token: Option<u64>,
}

/// Singleton model that tracks pane-scoped CLI agent state and plugin-enriched session context.
pub struct CLIAgentSessionsModel {
    sessions: HashMap<EntityId, CLIAgentSession>,
    /// Tracks (agent, remote_host) pairs where an auto plugin operation (install or update) has failed.
    /// Shared across all views so failure in one tab is reflected everywhere.
    plugin_auto_failures: HashSet<(CLIAgent, Option<String>)>,
    /// Ctrl-C pending-cancel state, keyed by terminal view. See `observe_ctrl_c_write`.
    ctrl_c_cancel_state: HashMap<EntityId, CtrlCCancelState>,
    /// Source of `CtrlCCancelState::armed_token` values. Monotonically increasing;
    /// never reused, so a stale callback can never alias a newer window.
    next_ctrl_c_token: u64,
    /// When each session's status last changed, keyed by terminal view.
    status_changed_at: HashMap<EntityId, DateTime<Local>>,
    provisional_work: HashMap<EntityId, ProvisionalWork>,
    next_provisional_token: u64,
    /// WarpOss: the live wait per session for the hook's count after a quiet stop.
    report_wait_tokens: HashMap<EntityId, u64>,
    next_report_wait_token: u64,
    /// WarpOss: some session reported through the background-tasks hook in this app run, so
    /// the hook is installed and new sessions wait for its count too.
    background_hook_seen: bool,
    /// WarpOss: sessions shown as "needs you" because Claude Code's own session file says it
    /// waits on a dialog that sends no hook (`set_external_wait`).
    external_waits: HashMap<EntityId, ExternalWait>,
}

/// A "needs you" set from Claude Code's session file, and what it replaced.
struct ExternalWait {
    restore: CLIAgentSessionStatus,
    restore_changed_at: Option<DateTime<Local>>,
    shown: CLIAgentSessionStatus,
}

impl Entity for CLIAgentSessionsModel {
    type Event = CLIAgentSessionsModelEvent;
}

impl SingletonEntity for CLIAgentSessionsModel {}

impl CLIAgentSessionsModel {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            plugin_auto_failures: HashSet::new(),
            ctrl_c_cancel_state: HashMap::new(),
            next_ctrl_c_token: 0,
            status_changed_at: HashMap::new(),
            provisional_work: HashMap::new(),
            next_provisional_token: 0,
            report_wait_tokens: HashMap::new(),
            next_report_wait_token: 0,
            background_hook_seen: false,
            external_waits: HashMap::new(),
        }
    }

    /// WarpOss: Claude Code waits on a dialog that fires no hook (a held message from another
    /// session, a permission prompt behind a foreground command), as its session file says.
    /// `waiting` is the dialog's label while the file says "waiting" (`updated_ms`: when it
    /// started), `None` once it says anything else. Entering shows "needs you" exactly like a
    /// hook's, announced once; leaving puts back what the events had shown, silently. A wait
    /// older than the session's last status change, or one the hooks already show, changes
    /// nothing.
    pub fn set_external_wait(
        &mut self,
        terminal_view_id: EntityId,
        waiting: Option<String>,
        updated_ms: u64,
        ctx: &mut ModelContext<Self>,
    ) {
        let changed_at = self.status_changed_at(terminal_view_id);
        let Some(session) = self.sessions.get_mut(&terminal_view_id) else {
            self.external_waits.remove(&terminal_view_id);
            return;
        };
        let has_background_work = session.session_context.running_background_tasks().is_some();
        let (status, announce, entered) = match waiting {
            Some(label) => {
                let known_newer = changed_at.is_some_and(|at| {
                    u64::try_from(at.timestamp_millis()).is_ok_and(|at| at > updated_ms)
                });
                if self.external_waits.contains_key(&terminal_view_id)
                    || matches!(session.status, CLIAgentSessionStatus::Blocked { .. })
                    || known_newer
                {
                    return;
                }
                let shown = CLIAgentSessionStatus::Blocked {
                    message: Some(label),
                    source: BlockedSource::NeedsInput,
                };
                let announce = status_change_is_announced(
                    &session.status,
                    has_background_work,
                    &shown,
                    has_background_work,
                );
                self.external_waits.insert(
                    terminal_view_id,
                    ExternalWait {
                        restore: session.status.clone(),
                        restore_changed_at: changed_at,
                        shown: shown.clone(),
                    },
                );
                session.status = shown.clone();
                self.status_changed_at
                    .insert(terminal_view_id, Local::now());
                (shown, announce, true)
            }
            None => {
                let Some(wait) = self.external_waits.remove(&terminal_view_id) else {
                    return;
                };
                // An event moved the session on meanwhile; it speaks for the row now.
                if session.status != wait.shown {
                    return;
                }
                session.status = wait.restore.clone();
                match wait.restore_changed_at {
                    Some(at) => self.status_changed_at.insert(terminal_view_id, at),
                    None => self.status_changed_at.remove(&terminal_view_id),
                };
                (wait.restore, false, false)
            }
        };
        // A provisional "working" must not fall back over the wait.
        if entered {
            self.clear_provisional_work(terminal_view_id);
        }
        let session = &self.sessions[&terminal_view_id];
        ctx.emit(CLIAgentSessionsModelEvent::StatusChanged {
            terminal_view_id,
            agent: session.agent,
            status,
            is_prompt_submit: false,
            session_context: Box::new(session.session_context.clone()),
            announce,
        });
    }

    /// When this session's status last changed, as seen by this model.
    pub fn status_changed_at(&self, terminal_view_id: EntityId) -> Option<DateTime<Local>> {
        self.status_changed_at.get(&terminal_view_id).copied()
    }

    fn clear_provisional_work(&mut self, terminal_view_id: EntityId) {
        if let Some(work) = self.provisional_work.remove(&terminal_view_id) {
            work.handle.abort();
        }
    }

    /// (Re)starts the window after which a provisionally working session falls back to `restore`.
    fn arm_provisional_work(
        &mut self,
        terminal_view_id: EntityId,
        restore: CLIAgentSessionStatus,
        restore_changed_at: Option<DateTime<Local>>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.clear_provisional_work(terminal_view_id);
        let token = self.next_provisional_token;
        self.next_provisional_token += 1;
        let handle = ctx.spawn_abortable(
            async move { warpui::r#async::Timer::after(PROVISIONAL_WORK_WINDOW).await },
            move |model, _, ctx| model.resolve_provisional_work(terminal_view_id, token, ctx),
            |_, _| {},
        );
        self.provisional_work.insert(
            terminal_view_id,
            ProvisionalWork {
                restore,
                restore_changed_at,
                token,
                handle,
            },
        );
    }

    fn resolve_provisional_work(
        &mut self,
        terminal_view_id: EntityId,
        token: u64,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self
            .provisional_work
            .get(&terminal_view_id)
            .is_some_and(|work| work.token == token)
        {
            return;
        }
        let Some(work) = self.provisional_work.remove(&terminal_view_id) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(&terminal_view_id) else {
            return;
        };
        if session.status != CLIAgentSessionStatus::InProgress {
            return;
        }
        session.status = work.restore.clone();
        let agent = session.agent;
        let session_context = Box::new(session.session_context.clone());
        match work.restore_changed_at {
            Some(at) => {
                self.status_changed_at.insert(terminal_view_id, at);
            }
            None => {
                self.status_changed_at.remove(&terminal_view_id);
            }
        }
        ctx.emit(CLIAgentSessionsModelEvent::StatusChanged {
            terminal_view_id,
            agent,
            status: work.restore,
            is_prompt_submit: false,
            session_context,
            // Falling back after stray tool events is not a turn ending; the real end was
            // announced (or deliberately not) when it happened.
            announce: false,
        });
    }

    pub fn session(&self, terminal_view_id: EntityId) -> Option<&CLIAgentSession> {
        self.sessions.get(&terminal_view_id)
    }

    /// Returns `true` if the rich input editor is currently open for this terminal.
    pub fn is_input_open(&self, terminal_view_id: EntityId) -> bool {
        self.sessions
            .get(&terminal_view_id)
            .is_some_and(|s| matches!(s.input_state, CLIAgentInputState::Open { .. }))
    }

    /// Registers a plugin-backed listener on the session for this terminal.
    ///
    /// If a session for the same agent already exists (e.g. created earlier by
    /// command detection), it is upgraded with the listener and plugin context.
    /// Otherwise a new session is created.
    ///
    /// The optional `cwd` / `project` / `session_id` fields supply initial
    /// context when available (e.g. from a `SessionStart` event). Passing
    /// `None` for all three is fine — happens when the plugin is installed
    /// mid-session and there is no start event to extract context from.
    #[allow(clippy::too_many_arguments)]
    pub fn register_listener(
        &mut self,
        terminal_view_id: EntityId,
        agent: CLIAgent,
        cwd: Option<String>,
        project: Option<String>,
        session_id: Option<String>,
        plugin_version: Option<String>,
        remote_host: Option<String>,
        should_auto_toggle_input: bool,
        initial_status: CLIAgentSessionStatus,
        listener: ModelHandle<CLIAgentSessionListener>,
        ctx: &mut ModelContext<Self>,
    ) {
        if let Some(session) = self
            .sessions
            .get_mut(&terminal_view_id)
            .filter(|s| s.agent == agent)
        {
            // Upgrade existing session with plugin context. WarpOss: the caller derives
            // `initial_status` from the event it registers from; the event then sets the time.
            // Always starting in progress left a Claude that only sent `idle_prompt` after a
            // restart "working" for good.
            session.status = initial_status;
            // Harp: what the title said before the plugin spoke (Claude's start-up spinner) is no
            // turn; the event this registers from decides.
            session.session_context.had_activity = false;
            self.status_changed_at.remove(&terminal_view_id);
            session.listener = Some(listener);
            session.plugin_version = plugin_version;
            session.remote_host = remote_host;
            session.should_auto_toggle_input = should_auto_toggle_input;
            session.session_context.cwd = cwd.or(session.session_context.cwd.take());
            session.session_context.project = project.or(session.session_context.project.take());
            session.session_context.session_id =
                session_id.or(session.session_context.session_id.take());
            return;
        }

        self.set_session(
            terminal_view_id,
            CLIAgentSession {
                agent,
                status: initial_status,
                session_context: CLIAgentSessionContext {
                    cwd,
                    project,
                    session_id,
                    ..Default::default()
                },
                input_state: CLIAgentInputState::Closed,
                should_auto_toggle_input,
                listener: Some(listener),
                plugin_version,
                remote_host,
                draft_text: None,
                custom_command_prefix: None,
                received_rich_notification: false,
            },
            ctx,
        );
        self.status_changed_at.remove(&terminal_view_id);
    }

    pub fn remove_session(&mut self, terminal_view_id: EntityId, ctx: &mut ModelContext<Self>) {
        self.abort_pending_cancel(terminal_view_id);
        self.ctrl_c_cancel_state.remove(&terminal_view_id);
        self.clear_provisional_work(terminal_view_id);
        self.status_changed_at.remove(&terminal_view_id);
        self.external_waits.remove(&terminal_view_id);
        if let Some(session) = self.sessions.remove(&terminal_view_id) {
            ctx.emit(CLIAgentSessionsModelEvent::Ended {
                terminal_view_id,
                agent: session.agent,
            });
        }
    }

    /// Updates the session's status and context from a parsed CLI agent event.
    /// Rich plugin events latch `received_rich_notification` so rich-status
    /// surfaces stay consistent even if the first event was not SessionStart.
    pub fn update_from_event(
        &mut self,
        terminal_view_id: EntityId,
        event: &CLIAgentEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.sessions.contains_key(&terminal_view_id) {
            return;
        }

        // Any plugin event other than `IdlePrompt` is evidence the CLI agent
        // process is still alive — an interrupt produces silence instead.
        // Disarm a pending Ctrl-C cancellation window so this event's own
        // status transition drives the session. `IdlePrompt` is excluded:
        // it means the CLI is sitting idle at its interactive prompt, which
        // is evidence of idleness rather than aliveness, so treating it as
        // disarming would let an idle notification that arrives instead of
        // a genuine `stop`/`stop_failure` after an interrupt silently defeat
        // the grace window, leaving the session stuck exactly like the bug
        // this feature exists to fix. `apply_event` still treats `IdlePrompt`
        // as a no-op for status, independent of this.
        // `BackgroundTasks` is excluded for the same reason: background agents can outlive an
        // interrupted foreground turn.
        if !matches!(
            event.event,
            CLIAgentEventType::IdlePrompt | CLIAgentEventType::BackgroundTasks { .. }
        ) {
            self.abort_pending_cancel(terminal_view_id);
        }
        if matches!(event.event, CLIAgentEventType::PromptSubmit) {
            self.ctrl_c_cancel_state
                .entry(terminal_view_id)
                .or_default()
                .has_seen_prompt_submit = true;
        }

        if matches!(event.event, CLIAgentEventType::BackgroundTasks { .. }) {
            self.background_hook_seen = true;
        }
        let session = self
            .sessions
            .get_mut(&terminal_view_id)
            .expect("session presence checked above");
        session.session_context.background_hook_installed |= self.background_hook_seen;
        if session.is_stale_background_report(event) {
            return;
        }

        match event.source {
            CLIAgentEventSource::RichPlugin => {
                session.received_rich_notification = true;
                session.session_context.status_from_title = false;
            }
            // Once a plugin reports, the title only repeats what it says, later and coarser.
            CLIAgentEventSource::TerminalTitle if session.has_status_plugin() => return,
            CLIAgentEventSource::TerminalTitle => session.session_context.status_from_title = true,
            CLIAgentEventSource::CodexOsc9Fallback => {}
        }

        let previous_status = session.status.clone();
        let had_background_work = session.session_context.running_background_tasks().is_some();
        let was_awaiting_report = session.session_context.awaiting_background_report.clone();
        let event_type = &event.event;
        // Interactive clients keep ignoring this event: only an unattended run has no one to
        // answer the prompt.
        let new_status = if matches!(event.event, CLIAgentEventType::NeedsInput)
            && !AppExecutionMode::as_ref(ctx).is_autonomous()
        {
            None
        } else {
            session.apply_event(event)
        };
        // An event that changes the status ends a wait read from the session file.
        if new_status.is_some() {
            self.external_waits.remove(&terminal_view_id);
        }
        let agent = session.agent;
        // WarpOss: a stop that came before its turn's hook count ends quietly and waits. The next
        // hook report settles it, a status change ends the wait, and without any report the wait
        // runs out (`resolve_background_report_wait`). Status-neutral events keep it.
        let settled_wait = was_awaiting_report
            .filter(|_| matches!(event_type, CLIAgentEventType::BackgroundTasks { .. }));
        if settled_wait.is_some()
            || (new_status.is_some() && !matches!(event_type, CLIAgentEventType::Stop))
        {
            session.session_context.awaiting_background_report = None;
        }
        let waits_for_report = matches!(event_type, CLIAgentEventType::Stop)
            && new_status.is_none()
            && session.session_context.awaiting_background_report.is_some();
        // WarpOss: a finished turn reads as working while background work runs, so the row's
        // time restarts whenever that work starts or ends, not only on foreground changes.
        let background_flipped = session.status == CLIAgentSessionStatus::Success
            && had_background_work != session.session_context.running_background_tasks().is_some();
        let is_tool_complete = matches!(event_type, CLIAgentEventType::ToolComplete);
        let settled_done = settled_wait.filter(|_| {
            session.status == CLIAgentSessionStatus::Success
                && session.session_context.running_background_tasks().is_none()
        });
        if waits_for_report {
            // The row shows done from this stop on; the announcement waits for the hook.
            self.status_changed_at.insert(terminal_view_id, Local::now());
        } else if let Some(before) = settled_done {
            // The hook says nothing runs: the quiet stop was the turn ending after all, announced
            // as that stop would have been (a turn after a Ctrl-C stays silent).
            ctx.emit(CLIAgentSessionsModelEvent::StatusChanged {
                terminal_view_id,
                agent,
                status: CLIAgentSessionStatus::Success,
                is_prompt_submit: false,
                session_context: Box::new(session.session_context.clone()),
                announce: status_change_is_announced(
                    &before,
                    false,
                    &CLIAgentSessionStatus::Success,
                    false,
                ),
            });
        } else if new_status.is_none() && background_flipped {
            self.status_changed_at.insert(terminal_view_id, Local::now());
            // WarpOss: background work running out after the turn turns the row from working
            // to done without a status change of its own; that is news like a turn ending.
            if had_background_work {
                ctx.emit(CLIAgentSessionsModelEvent::StatusChanged {
                    terminal_view_id,
                    agent,
                    status: CLIAgentSessionStatus::Success,
                    is_prompt_submit: false,
                    session_context: Box::new(session.session_context.clone()),
                    announce: true,
                });
            }
        }
        if let Some(new_status) = new_status {
            let announce = status_change_is_announced(
                &previous_status,
                had_background_work,
                &new_status,
                session.session_context.running_background_tasks().is_some(),
            );
            ctx.emit(CLIAgentSessionsModelEvent::StatusChanged {
                terminal_view_id,
                agent,
                status: new_status.clone(),
                is_prompt_submit: matches!(event_type, CLIAgentEventType::PromptSubmit),
                session_context: Box::new(session.session_context.clone()),
                announce,
            });
            let reopened_finished_turn = is_tool_complete
                && new_status == CLIAgentSessionStatus::InProgress
                && matches!(
                    previous_status,
                    CLIAgentSessionStatus::Success | CLIAgentSessionStatus::Failed { .. }
                );
            if reopened_finished_turn {
                let restore_changed_at = self.status_changed_at(terminal_view_id);
                self.arm_provisional_work(terminal_view_id, previous_status, restore_changed_at, ctx);
            } else {
                self.clear_provisional_work(terminal_view_id);
            }
            self.status_changed_at.insert(terminal_view_id, Local::now());
        } else if is_tool_complete {
            // Continued tool activity keeps a provisionally working session working.
            if let Some(work) = self.provisional_work.remove(&terminal_view_id) {
                work.handle.abort();
                self.arm_provisional_work(
                    terminal_view_id,
                    work.restore,
                    work.restore_changed_at,
                    ctx,
                );
            }
        }

        if matches!(
            event_type,
            CLIAgentEventType::SessionStart
                | CLIAgentEventType::PromptSubmit
                | CLIAgentEventType::ToolComplete
                | CLIAgentEventType::BackgroundTasks { .. }
        ) {
            ctx.emit(CLIAgentSessionsModelEvent::SessionUpdated {
                terminal_view_id,
                agent,
            });
        }
        if waits_for_report {
            self.clear_provisional_work(terminal_view_id);
            self.arm_background_report_wait(terminal_view_id, ctx);
        }
    }

    /// WarpOss: starts the wait for the hook's count after a quiet stop; see
    /// `CLIAgentSessionContext::awaiting_background_report`.
    fn arm_background_report_wait(
        &mut self,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        let token = self.next_report_wait_token;
        self.next_report_wait_token += 1;
        self.report_wait_tokens.insert(terminal_view_id, token);
        // Stale callbacks are dropped by the token, so the handle need not be kept.
        let _ = ctx.spawn_abortable(
            async move { warpui::r#async::Timer::after(BACKGROUND_REPORT_WAIT).await },
            move |model, _, ctx| model.resolve_background_report_wait(terminal_view_id, token, ctx),
            |_, _| {},
        );
    }

    /// No hook report came after a quiet stop: nothing is known to run, so the turn ended.
    fn resolve_background_report_wait(
        &mut self,
        terminal_view_id: EntityId,
        token: u64,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.report_wait_tokens.get(&terminal_view_id) != Some(&token) {
            return;
        }
        self.report_wait_tokens.remove(&terminal_view_id);
        let Some(session) = self.sessions.get_mut(&terminal_view_id) else {
            return;
        };
        let Some(before) = session.session_context.awaiting_background_report.take() else {
            return;
        };
        if session.status != CLIAgentSessionStatus::Success {
            return;
        }
        ctx.emit(CLIAgentSessionsModelEvent::StatusChanged {
            terminal_view_id,
            agent: session.agent,
            status: CLIAgentSessionStatus::Success,
            is_prompt_submit: false,
            session_context: Box::new(session.session_context.clone()),
            announce: status_change_is_announced(
                &before,
                false,
                &CLIAgentSessionStatus::Success,
                false,
            ),
        });
    }

    /// Observes a Ctrl-C byte (`0x03`) written to this session's PTY.
    ///
    /// This is observation only: the caller is responsible for forwarding the
    /// byte to the PTY unchanged and immediately, regardless of what this
    /// does. If the session is currently interruptible — `InProgress` or
    /// `Blocked`, rich-status-capable (excludes the Codex OSC 9 fallback),
    /// and has seen at least one `prompt_submit` (guarding against the
    /// optimistic `InProgress` set at registration, before any turn has
    /// started) — arms a grace window after which the session resolves to
    /// `Cancelled` if no disarming plugin activity arrives first (any event
    /// except `IdlePrompt`, which is evidence of idleness rather than
    /// aliveness; see `update_from_event`). A second Ctrl-C while a window
    /// is already armed reuses it rather than resetting the clock.
    pub fn observe_ctrl_c_write(
        &mut self,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.observe_ctrl_c_write_with_window(terminal_view_id, CTRL_C_CANCEL_WINDOW, ctx);
    }

    fn observe_ctrl_c_write_with_window(
        &mut self,
        terminal_view_id: EntityId,
        window: Duration,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(session) = self.sessions.get(&terminal_view_id) else {
            return;
        };
        let can_arm = matches!(
            session.status,
            CLIAgentSessionStatus::InProgress | CLIAgentSessionStatus::Blocked { .. }
        ) && session.supports_rich_status()
            && self
                .ctrl_c_cancel_state
                .get(&terminal_view_id)
                .is_some_and(|state| state.has_seen_prompt_submit);
        if !can_arm {
            return;
        }
        if self
            .ctrl_c_cancel_state
            .get(&terminal_view_id)
            .is_some_and(|state| state.pending_cancel.is_some())
        {
            return;
        }

        let token = self.next_ctrl_c_token;
        self.next_ctrl_c_token += 1;
        let state = self
            .ctrl_c_cancel_state
            .entry(terminal_view_id)
            .or_default();
        let handle = ctx.spawn_abortable(
            async move { warpui::r#async::Timer::after(window).await },
            move |model, _, ctx| model.resolve_pending_cancel(terminal_view_id, token, ctx),
            |_, _| {},
        );
        state.pending_cancel = Some(handle);
        state.armed_token = Some(token);
    }

    /// Called when a session's pending-cancel window lapses with no
    /// disarming plugin event. Transitions the session to `Cancelled` unless
    /// `token` no longer matches the currently armed window — meaning this
    /// callback was already queued (post `Timer::after` completion, pre-poll)
    /// when the window was disarmed, replaced, or removed, and
    /// `SpawnedFutureHandle::abort` did not take effect in time to stop it.
    fn resolve_pending_cancel(
        &mut self,
        terminal_view_id: EntityId,
        token: u64,
        ctx: &mut ModelContext<Self>,
    ) {
        let owns_current_window = self
            .ctrl_c_cancel_state
            .get_mut(&terminal_view_id)
            .is_some_and(|state| {
                if state.armed_token != Some(token) {
                    return false;
                }
                state.pending_cancel = None;
                state.armed_token = None;
                true
            });
        if !owns_current_window {
            return;
        }
        self.force_cancel(terminal_view_id, ctx);
    }

    /// Aborts and clears any armed pending-cancel window for this terminal.
    /// Invalidates the token so a callback already queued when the abort
    /// fires too late to matter (see `resolve_pending_cancel`) is a no-op.
    fn abort_pending_cancel(&mut self, terminal_view_id: EntityId) {
        if let Some(state) = self.ctrl_c_cancel_state.get_mut(&terminal_view_id) {
            state.armed_token = None;
            if let Some(handle) = state.pending_cancel.take() {
                handle.abort();
            }
        }
    }

    /// Sets `status` to `Cancelled` and emits `StatusChanged`, unless the
    /// session has already moved past `InProgress`/`Blocked` or no longer
    /// exists.
    fn force_cancel(&mut self, terminal_view_id: EntityId, ctx: &mut ModelContext<Self>) {
        let Some(session) = self.sessions.get_mut(&terminal_view_id) else {
            return;
        };
        if !matches!(
            session.status,
            CLIAgentSessionStatus::InProgress | CLIAgentSessionStatus::Blocked { .. }
        ) {
            return;
        }

        session.status = CLIAgentSessionStatus::Cancelled;
        let agent = session.agent;
        let session_context = Box::new(session.session_context.clone());
        self.clear_provisional_work(terminal_view_id);
        self.status_changed_at.insert(terminal_view_id, Local::now());
        ctx.emit(CLIAgentSessionsModelEvent::StatusChanged {
            terminal_view_id,
            agent,
            status: CLIAgentSessionStatus::Cancelled,
            is_prompt_submit: false,
            session_context,
            // The user pressed Ctrl-C in this tab.
            announce: false,
        });
    }

    /// Whether Ctrl-C cancellation has already resolved (`Cancelled`) or is
    /// still pending (the grace window is armed) for this session. Only
    /// used by tests.
    #[cfg(test)]
    pub(crate) fn has_pending_or_resolved_ctrl_c_cancel(&self, terminal_view_id: EntityId) -> bool {
        if matches!(
            self.sessions.get(&terminal_view_id).map(|s| &s.status),
            Some(CLIAgentSessionStatus::Cancelled)
        ) {
            return true;
        }
        self.ctrl_c_cancel_state
            .get(&terminal_view_id)
            .is_some_and(|state| state.pending_cancel.is_some())
    }

    pub fn open_input(
        &mut self,
        terminal_view_id: EntityId,
        entrypoint: CLIAgentInputEntrypoint,
        previous_input_config: InputConfig,
        previous_was_lock_set_with_empty_buffer: bool,
        should_auto_toggle_input: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(session) = self.sessions.get_mut(&terminal_view_id) else {
            return;
        };

        let previous_input_state = session.input_state;
        session.input_state = CLIAgentInputState::Open {
            entrypoint,
            previous_input_config,
            previous_was_lock_set_with_empty_buffer,
        };
        session.should_auto_toggle_input = should_auto_toggle_input;

        ctx.emit(CLIAgentSessionsModelEvent::InputSessionChanged {
            terminal_view_id,
            agent: session.agent,
            previous_input_state,
            new_input_state: session.input_state,
        });
    }

    pub fn close_input(
        &mut self,
        terminal_view_id: EntityId,
        should_auto_toggle_input: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(session) = self.sessions.get_mut(&terminal_view_id) else {
            return;
        };
        if session.input_state == CLIAgentInputState::Closed {
            return;
        }

        let previous_input_state = session.input_state;
        session.input_state = CLIAgentInputState::Closed;
        session.should_auto_toggle_input = should_auto_toggle_input;
        ctx.emit(CLIAgentSessionsModelEvent::InputSessionChanged {
            terminal_view_id,
            agent: session.agent,
            previous_input_state,
            new_input_state: CLIAgentInputState::Closed,
        });
    }

    pub fn set_session(
        &mut self,
        terminal_view_id: EntityId,
        session: CLIAgentSession,
        ctx: &mut ModelContext<Self>,
    ) {
        let agent = session.agent;
        // Close any open rich input before replacing, so subscribers can
        // restore input config before the session ends.
        self.close_input(terminal_view_id, false, ctx);
        // A fresh session must re-observe `prompt_submit` before Ctrl-C can
        // arm, and any pending window belonged to the session being replaced.
        self.abort_pending_cancel(terminal_view_id);
        self.ctrl_c_cancel_state.remove(&terminal_view_id);
        self.clear_provisional_work(terminal_view_id);
        self.status_changed_at.insert(terminal_view_id, Local::now());
        if let Some(old) = self.sessions.insert(terminal_view_id, session) {
            ctx.emit(CLIAgentSessionsModelEvent::Ended {
                terminal_view_id,
                agent: old.agent,
            });
        }

        ctx.emit(CLIAgentSessionsModelEvent::Started {
            terminal_view_id,
            agent,
        });
    }

    /// Records that an auto plugin operation (install or update) failed for the given agent/host.
    /// `remote_host` is `None` for local sessions, `Some("user@hostname")` for remote.
    #[cfg(not(target_family = "wasm"))]
    pub fn record_plugin_auto_failure(&mut self, agent: CLIAgent, remote_host: Option<String>) {
        self.plugin_auto_failures.insert((agent, remote_host));
    }

    /// Saves draft text from the rich input composer for the given terminal.
    /// Stores `None` for empty or whitespace-only text.
    pub fn set_draft(&mut self, terminal_view_id: EntityId, text: String) {
        if let Some(session) = self.sessions.get_mut(&terminal_view_id) {
            session.draft_text = if text.trim().is_empty() {
                None
            } else {
                Some(text)
            };
        }
    }

    /// Clears any saved draft text for the given terminal.
    pub fn clear_draft(&mut self, terminal_view_id: EntityId) {
        if let Some(session) = self.sessions.get_mut(&terminal_view_id) {
            session.draft_text = None;
        }
    }

    /// Returns and clears the draft text for the given terminal, if any.
    pub fn take_draft(&mut self, terminal_view_id: EntityId) -> Option<String> {
        self.sessions
            .get_mut(&terminal_view_id)
            .and_then(|s| s.draft_text.take())
    }

    /// Whether an auto plugin operation has previously failed for this agent on this host.
    pub fn has_plugin_auto_failed(&self, agent: CLIAgent, remote_host: &Option<String>) -> bool {
        self.plugin_auto_failures
            .contains(&(agent, remote_host.clone()))
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

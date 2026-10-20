use std::time::Duration;

use warp_core::execution_mode::{AppExecutionMode, ExecutionMode};
use warpui::r#async::Timer;
use warpui::{App, EntityId};

use super::event::{
    CLIAgentEvent, CLIAgentEventPayload, CLIAgentEventSource, CLIAgentEventType, parse_event,
};
use super::{
    BlockedSource, CLIAgentInputEntrypoint, CLIAgentInputState, CLIAgentSession,
    CLIAgentSessionContext, CLIAgentSessionStatus, CLIAgentSessionsModel,
    CLIAgentSessionsModelEvent,
};
use crate::ai::blocklist::{InputConfig, InputType};
use crate::terminal::CLIAgent;

#[test]
fn parse_stop_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"write a haiku","response":"Memory is safe","transcript_path":"/tmp/t.jsonl"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.v, 1);
    assert_eq!(notif.agent, CLIAgent::Claude);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert_eq!(notif.session_id.as_deref(), Some("abc"));
    assert_eq!(notif.cwd.as_deref(), Some("/tmp/proj"));
    assert_eq!(notif.project.as_deref(), Some("proj"));
    assert_eq!(notif.payload.query.as_deref(), Some("write a haiku"));
    assert_eq!(notif.payload.response.as_deref(), Some("Memory is safe"));
    assert_eq!(
        notif.payload.transcript_path.as_deref(),
        Some("/tmp/t.jsonl")
    );
}

#[test]
fn cli_agent_session_context_title_like_text_uses_trimmed_summary() {
    let context = CLIAgentSessionContext {
        summary: Some("  Reviewing changes  ".to_string()),
        query: Some("Latest prompt".to_string()),
        ..Default::default()
    };

    assert_eq!(
        context.title_like_text(),
        Some("Reviewing changes".to_string())
    );
}

#[test]
fn cli_agent_session_context_latest_user_prompt_uses_trimmed_query() {
    let context = CLIAgentSessionContext {
        summary: Some("Reviewing changes".to_string()),
        query: Some("  Latest prompt  ".to_string()),
        ..Default::default()
    };

    assert_eq!(
        context.latest_user_prompt(),
        Some("Latest prompt".to_string())
    );
}

#[test]
fn cli_agent_session_context_title_helpers_ignore_empty_text() {
    let context = CLIAgentSessionContext {
        summary: Some("  ".to_string()),
        query: Some("".to_string()),
        ..Default::default()
    };

    assert_eq!(context.title_like_text(), None);
    assert_eq!(context.latest_user_prompt(), None);
}

#[test]
fn parse_permission_request_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"permission_request","session_id":"abc","cwd":"/tmp/proj","project":"proj","summary":"Wants to run Bash: rm -rf /tmp","tool_name":"Bash","tool_input":{"command":"rm -rf /tmp"}}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::PermissionRequest);
    assert_eq!(
        notif.payload.summary.as_deref(),
        Some("Wants to run Bash: rm -rf /tmp")
    );
    assert_eq!(notif.payload.tool_name.as_deref(), Some("Bash"));
    assert_eq!(
        notif.payload.tool_input_preview.as_deref(),
        Some("rm -rf /tmp")
    );
}

#[test]
fn parse_permission_request_with_file_path() {
    let body = r#"{"v":1,"agent":"claude","event":"permission_request","session_id":"abc","cwd":"/tmp","project":"tmp","tool_name":"Write","tool_input":{"file_path":"/tmp/test.py","content":"print('hi')"}}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(
        notif.payload.tool_input_preview.as_deref(),
        Some("/tmp/test.py")
    );
}

#[test]
fn parse_idle_prompt_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"idle_prompt","session_id":"abc","cwd":"/tmp","project":"tmp","summary":"Claude is waiting for your input"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::IdlePrompt);
    assert_eq!(
        notif.payload.summary.as_deref(),
        Some("Claude is waiting for your input")
    );
}

#[test]
fn parse_agent_needs_input_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"agent_needs_input","session_id":"abc","cwd":"/tmp","project":"tmp","summary":"Some dialog text"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::NeedsInput);
    assert_eq!(notif.payload.summary.as_deref(), Some("Some dialog text"));
}

#[test]
fn parse_session_start_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"session_start","session_id":"abc","cwd":"/tmp","project":"tmp","plugin_version":"1.1.0"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::SessionStart);
    assert_eq!(notif.payload.plugin_version.as_deref(), Some("1.1.0"));
}

#[test]
fn returns_none_for_wrong_sentinel() {
    let body = r#"{"v":1,"agent":"claude","event":"stop"}"#;
    assert!(parse_event(Some("Claude Code"), body).is_none());
}

#[test]
fn returns_none_for_missing_title() {
    let body = r#"{"v":1,"agent":"claude","event":"stop"}"#;
    assert!(parse_event(None, body).is_none());
}

#[test]
fn returns_none_for_invalid_json() {
    assert!(parse_event(Some("warp://cli-agent"), "not json").is_none());
}

#[test]
fn handles_unknown_event_type() {
    let body = r#"{"v":1,"agent":"claude","event":"some_future_event"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();
    assert_eq!(
        notif.event,
        CLIAgentEventType::Unknown("some_future_event".to_string())
    );
}

#[test]
fn handles_missing_optional_fields() {
    let body = r#"{"event":"stop"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.v, 1);
    assert_eq!(notif.agent, CLIAgent::Unknown);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert!(notif.session_id.is_none());
    assert!(notif.cwd.is_none());
    assert!(notif.project.is_none());
    assert!(notif.payload.query.is_none());
}

#[test]
fn handles_special_characters_in_values() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","query":"what does \"hello\" mean?","response":"It means greeting. Use: printf(\"hello\")"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(
        notif.payload.query.as_deref(),
        Some("what does \"hello\" mean?")
    );
    assert_eq!(
        notif.payload.response.as_deref(),
        Some("It means greeting. Use: printf(\"hello\")")
    );
}

#[test]
fn rejects_unsupported_schema_version() {
    let body = r#"{"v":2,"agent":"claude","event":"stop"}"#;
    assert!(parse_event(Some("warp://cli-agent"), body).is_none());
}

#[test]
fn defaults_to_v1_when_version_missing() {
    let body = r#"{"agent":"claude","event":"stop","query":"hi"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();
    assert_eq!(notif.v, 1);
    assert_eq!(notif.payload.query.as_deref(), Some("hi"));
}

#[test]
fn explicit_v1_parses_correctly() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","query":"test"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();
    assert_eq!(notif.v, 1);
    assert_eq!(notif.payload.query.as_deref(), Some("test"));
}

#[test]
fn parse_prompt_submit_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"prompt_submit","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"fix the bug"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::PromptSubmit);
    assert_eq!(notif.payload.query.as_deref(), Some("fix the bug"));
}

#[test]
fn parse_tool_complete_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"tool_complete","session_id":"abc","cwd":"/tmp/proj","project":"proj","tool_name":"Bash"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::ToolComplete);
    assert_eq!(notif.payload.tool_name.as_deref(), Some("Bash"));
}

#[test]
fn parse_auggie_stop_notification() {
    // Mirrors what the community auggie-warp plugin emits on the Stop hook.
    let body = r#"{"v":1,"agent":"auggie","event":"stop","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"write a haiku","response":"Memory is safe"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.agent, CLIAgent::Auggie);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert_eq!(notif.payload.query.as_deref(), Some("write a haiku"));
    assert_eq!(notif.payload.response.as_deref(), Some("Memory is safe"));
}

#[test]
fn parse_pi_stop_notification() {
    // Mirrors what the community pi-mono plugin emits on the Stop hook —
    // matches the Auggie shape and uses `"agent":"pi"`, which `resolve_agent`
    // already maps to `CLIAgent::Pi` via `command_prefix()`.
    let body = r#"{"v":1,"agent":"pi","event":"stop","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"write a haiku","response":"Memory is safe"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.agent, CLIAgent::Pi);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert_eq!(notif.payload.query.as_deref(), Some("write a haiku"));
    assert_eq!(notif.payload.response.as_deref(), Some("Memory is safe"));
}

#[test]
fn parse_droid_stop_notification() {
    // Droid is already a known CLI agent, so structured OSC 777 events using
    // `"agent":"droid"` should resolve through the existing command prefix
    // parser without any Droid-specific parser logic.
    let body = r#"{"v":1,"agent":"droid","event":"stop","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"write a haiku","response":"Memory is safe"}"#;
    let notif = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(notif.agent, CLIAgent::Droid);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert_eq!(notif.payload.query.as_deref(), Some("write a haiku"));
    assert_eq!(notif.payload.response.as_deref(), Some("Memory is safe"));
}

#[test]
fn apply_event_preserves_input_session() {
    let input_state = CLIAgentInputState::Open {
        entrypoint: CLIAgentInputEntrypoint::CtrlG,
        previous_input_config: InputConfig {
            input_type: InputType::Shell,
            is_locked: false,
        },
        previous_was_lock_set_with_empty_buffer: true,
    };
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state,
        should_auto_toggle_input: false,
        listener: None,
        remote_host: None,
        plugin_version: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::PermissionRequest,
        session_id: Some("abc".to_string()),
        cwd: Some("/tmp/proj".to_string()),
        project: Some("proj".to_string()),
        payload: CLIAgentEventPayload {
            summary: Some("Needs approval".to_string()),
            ..Default::default()
        },
    };

    session.apply_event(&event);

    assert_eq!(session.input_state, input_state);
}

#[test]
fn is_remote_returns_true_when_remote_host_is_set() {
    let session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: Some("user@devbox".to_owned()),
        custom_command_prefix: None,
        received_rich_notification: false,
    };
    assert!(session.is_remote());
}

#[test]
fn is_remote_returns_false_when_remote_host_is_none() {
    let session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        remote_host: None,
        plugin_version: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };
    assert!(!session.is_remote());
}

#[test]
fn local_failure_is_shared_across_local_sessions() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, None);

    assert!(model.has_plugin_auto_failed(CLIAgent::Claude, &None));
}

#[test]
fn local_failure_does_not_affect_remote_host() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, None);

    let remote = Some("user@devbox".to_owned());
    assert!(!model.has_plugin_auto_failed(CLIAgent::Claude, &remote));
}

#[test]
fn remote_failure_does_not_affect_local() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, Some("user@devbox".to_owned()));

    assert!(!model.has_plugin_auto_failed(CLIAgent::Claude, &None));
}

#[test]
fn remote_failures_are_independent_per_host() {
    let mut model = CLIAgentSessionsModel::new();

    let host_a = Some("user@host-a".to_owned());
    let host_b = Some("user@host-b".to_owned());

    model.record_plugin_auto_failure(CLIAgent::Claude, host_a.clone());

    assert!(model.has_plugin_auto_failed(CLIAgent::Claude, &host_a));
    assert!(!model.has_plugin_auto_failed(CLIAgent::Claude, &host_b));
}

#[test]
fn failure_tracking_is_independent_per_agent() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, None);

    assert!(model.has_plugin_auto_failed(CLIAgent::Claude, &None));
    assert!(!model.has_plugin_auto_failed(CLIAgent::Gemini, &None));
}

#[test]
fn session_start_sets_plugin_version() {
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::SessionStart,
        session_id: Some("abc".to_owned()),
        cwd: Some("/tmp".to_owned()),
        project: Some("proj".to_owned()),
        payload: CLIAgentEventPayload {
            plugin_version: Some("1.5.0".to_owned()),
            ..Default::default()
        },
    };

    session.apply_event(&event);
    assert_eq!(session.plugin_version.as_deref(), Some("1.5.0"));
}

#[test]
fn session_start_without_plugin_version_leaves_none() {
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::SessionStart,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };

    session.apply_event(&event);
    assert_eq!(session.plugin_version, None);
}

#[test]
fn codex_session_not_rich_until_rich_notification() {
    // Codex's OSC 9 fallback never sets `received_rich_notification`, so the
    // session must not claim rich status even when a fallback listener exists.
    let mut session = CLIAgentSession {
        agent: CLIAgent::Codex,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        remote_host: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };
    assert!(!session.supports_rich_status());

    // A structured OSC 777 notification latches the flag -> rich status.
    session.received_rich_notification = true;
    assert!(session.supports_rich_status());
}

#[test]
fn non_codex_session_rich_after_rich_notification() {
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        remote_host: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };
    // No listener and no rich notification yet.
    assert!(!session.supports_rich_status());

    session.received_rich_notification = true;
    assert!(session.supports_rich_status());
}

/// Constructs a session with permission-scoped state already populated, as if
/// a `PermissionRequest` had just been received and the agent is now Blocked.
/// Used by the GH-9525 regression tests below.
fn blocked_claude_session_with_permission_state() -> CLIAgentSession {
    CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::Blocked {
            message: Some("Wants to run bash: rm -rf /tmp".to_owned()),
            source: BlockedSource::PermissionRequest,
        },
        session_context: CLIAgentSessionContext {
            summary: Some("Wants to run bash: rm -rf /tmp".to_owned()),
            tool_name: Some("Bash".to_owned()),
            tool_input_preview: Some("rm -rf /tmp".to_owned()),
            ..Default::default()
        },
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    }
}

#[test]
fn stop_clears_permission_scoped_state() {
    // GH-9525: after a PermissionRequest sets `summary`, the Stop event must
    // clear it. Otherwise the tab title falls back to the stale permission
    // text instead of reflecting the now-completed session.
    let mut session = blocked_claude_session_with_permission_state();

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::Stop,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload {
            query: Some("write a haiku".to_owned()),
            response: Some("Memory is safe".to_owned()),
            ..Default::default()
        },
    };

    session.apply_event(&event);

    assert_eq!(session.session_context.summary, None);
    assert_eq!(session.session_context.tool_name, None);
    assert_eq!(session.session_context.tool_input_preview, None);
    assert_eq!(
        session.session_context.query.as_deref(),
        Some("write a haiku"),
    );
    assert_eq!(
        session.session_context.response.as_deref(),
        Some("Memory is safe"),
    );
    assert!(matches!(session.status, CLIAgentSessionStatus::Success));
}

#[test]
fn permission_replied_clears_permission_scoped_state() {
    // When the user replies to a permission prompt the agent transitions back
    // to InProgress; the now-stale summary/tool fields must be cleared so they
    // don't leak into UI surfaces during the next turn.
    let mut session = blocked_claude_session_with_permission_state();

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::PermissionReplied,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };

    session.apply_event(&event);

    assert_eq!(session.session_context.summary, None);
    assert_eq!(session.session_context.tool_name, None);
    assert_eq!(session.session_context.tool_input_preview, None);
    assert!(matches!(session.status, CLIAgentSessionStatus::InProgress));
}

#[test]
fn prompt_submit_clears_permission_scoped_state() {
    // PromptSubmit already clears `response`; clearing the permission-scoped
    // fields keeps the same hygiene if the user manages to start a new turn
    // while permission state is still populated (e.g. an abandoned permission
    // flow that was not closed by an explicit PermissionReplied).
    let mut session = blocked_claude_session_with_permission_state();
    session.session_context.response = Some("stale response".to_owned());

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::PromptSubmit,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload {
            query: Some("next prompt".to_owned()),
            ..Default::default()
        },
    };

    session.apply_event(&event);

    assert_eq!(session.session_context.summary, None);
    assert_eq!(session.session_context.tool_name, None);
    assert_eq!(session.session_context.tool_input_preview, None);
    assert_eq!(session.session_context.response, None);
    assert_eq!(
        session.session_context.query.as_deref(),
        Some("next prompt")
    );
    assert!(matches!(session.status, CLIAgentSessionStatus::InProgress));
}

#[test]
fn tool_complete_clears_permission_scoped_state() {
    // GH-11082: answering an AskUserQuestion emits only ToolComplete (the
    // plugin sends no PermissionReplied for it), so the Blocked -> InProgress
    // transition here must also clear the stale summary. Otherwise the tab
    // title keeps showing "Wants to run AskUserQuestion: ..." until the next
    // prompt or Stop.
    let mut session = blocked_claude_session_with_permission_state();

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::ToolComplete,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };

    session.apply_event(&event);

    assert_eq!(session.session_context.summary, None);
    assert_eq!(session.session_context.tool_name, None);
    assert_eq!(session.session_context.tool_input_preview, None);
    assert!(matches!(session.status, CLIAgentSessionStatus::InProgress));
}

#[test]
fn permission_request_still_populates_summary_and_tool_fields() {
    // Sanity: clearing permission-scoped state on Stop/Reply/Submit must not
    // also break the PermissionRequest path that initially populates them.
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: None,
        custom_command_prefix: None,
        received_rich_notification: false,
    };

    let event = CLIAgentEvent {
        source: CLIAgentEventSource::RichPlugin,
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::PermissionRequest,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload {
            summary: Some("Wants to run bash: rm -rf /tmp".to_owned()),
            tool_name: Some("Bash".to_owned()),
            tool_input_preview: Some("rm -rf /tmp".to_owned()),
            ..Default::default()
        },
    };

    session.apply_event(&event);

    assert_eq!(
        session.session_context.summary.as_deref(),
        Some("Wants to run bash: rm -rf /tmp"),
    );
    assert_eq!(session.session_context.tool_name.as_deref(), Some("Bash"));
    assert_eq!(
        session.session_context.tool_input_preview.as_deref(),
        Some("rm -rf /tmp"),
    );
    assert!(matches!(
        session.status,
        CLIAgentSessionStatus::Blocked { .. },
    ));
}

#[test]
fn needs_input_blocks_session_with_the_event_summary() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);

    let event = CLIAgentEvent {
        payload: CLIAgentEventPayload {
            summary: Some("Some dialog text".to_owned()),
            ..Default::default()
        },
        ..rich_event(CLIAgentEventType::NeedsInput)
    };

    let new_status = session.apply_event(&event);

    let expected = CLIAgentSessionStatus::Blocked {
        message: Some("Some dialog text".to_owned()),
        source: BlockedSource::NeedsInput,
    };
    assert_eq!(new_status, Some(expected.clone()));
    assert_eq!(session.status, expected);
    assert!(session.is_blocked_on_needs_input());
}

#[test]
fn needs_input_without_a_summary_blocks_session_with_the_generic_message() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);

    let new_status = session.apply_event(&rich_event(CLIAgentEventType::NeedsInput));

    let expected = CLIAgentSessionStatus::Blocked {
        message: Some("Agent requires user input".to_owned()),
        source: BlockedSource::NeedsInput,
    };
    assert_eq!(new_status, Some(expected));
    assert!(session.is_blocked_on_needs_input());
}

#[test]
fn blocked_status_records_the_event_that_caused_it() {
    for (event, expected_source) in [
        (
            CLIAgentEventType::PermissionRequest,
            BlockedSource::PermissionRequest,
        ),
        (
            CLIAgentEventType::QuestionAsked,
            BlockedSource::QuestionAsked,
        ),
        (CLIAgentEventType::NeedsInput, BlockedSource::NeedsInput),
    ] {
        let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);

        session.apply_event(&rich_event(event));

        let CLIAgentSessionStatus::Blocked { source, .. } = session.status else {
            panic!("expected a blocked status");
        };
        assert_eq!(source, expected_source);
        assert_eq!(
            session.is_blocked_on_needs_input(),
            expected_source == BlockedSource::NeedsInput
        );
    }
}

#[test]
fn permission_request_after_needs_input_is_no_longer_blocked_on_needs_input() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&rich_event(CLIAgentEventType::NeedsInput));

    session.apply_event(&rich_event(CLIAgentEventType::PermissionRequest));

    assert!(!session.is_blocked_on_needs_input());
}

#[test]
fn tool_complete_after_needs_input_returns_session_to_in_progress() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&rich_event(CLIAgentEventType::NeedsInput));

    session.apply_event(&rich_event(CLIAgentEventType::ToolComplete));

    assert_eq!(session.status, CLIAgentSessionStatus::InProgress);
    assert!(!session.is_blocked_on_needs_input());
}

#[test]
fn idle_prompt_after_needs_input_keeps_session_blocked() {
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.apply_event(&rich_event(CLIAgentEventType::NeedsInput));

    let new_status = session.apply_event(&rich_event(CLIAgentEventType::IdlePrompt));

    assert_eq!(new_status, None);
    assert!(matches!(
        session.status,
        CLIAgentSessionStatus::Blocked { .. }
    ));
    assert!(session.is_blocked_on_needs_input());
}

fn assert_needs_input_blocks_session_in_mode(mode: ExecutionMode, expect_blocked: bool) {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(mode, false, ctx));
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });

        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::NeedsInput), ctx);
        });

        let status = model.read(&app, |m, _| m.session(view_id).map(|s| s.status.clone()));
        assert_eq!(
            matches!(status, Some(CLIAgentSessionStatus::Blocked { .. })),
            expect_blocked
        );
    });
}

#[test]
fn needs_input_blocks_autonomous_sessions() {
    assert_needs_input_blocks_session_in_mode(ExecutionMode::Sdk, true);
}

#[test]
fn needs_input_is_ignored_in_the_desktop_app() {
    assert_needs_input_blocks_session_in_mode(ExecutionMode::App, false);
}

#[test]
fn stop_with_only_the_plugin_count_ends_the_turn_quietly_without_background_work() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","query":"build it","response":"Build started in the background.","background_task_count":1,"session_cron_count":2}"#;
    let event = parse_event(Some("warp://cli-agent"), body).unwrap();
    assert_eq!(event.payload.pending_background_work_count, Some(1));
    assert!(event.payload.has_pending_background_work());
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);
    session.session_context.background_hook_installed = true;

    // The plugin's count lists monitors and finished entries too, so on its own it never keeps
    // the row working; the stop waits quietly for the hook's count.
    assert_eq!(session.apply_event(&event), None);

    assert_eq!(session.status, CLIAgentSessionStatus::Success);
    assert_eq!(session.session_context.running_background_tasks(), None);
    assert!(session.session_context.awaiting_background_report.is_some());
    assert_eq!(
        session.session_context.response.as_deref(),
        Some("Build started in the background.")
    );
}

#[test]
fn stop_without_pending_background_work_succeeds() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","background_task_count":0,"session_cron_count":0}"#;
    let event = parse_event(Some("warp://cli-agent"), body).unwrap();
    let mut session = cli_agent_session(CLIAgentSessionStatus::InProgress, true);

    assert_eq!(
        session.apply_event(&event),
        Some(CLIAgentSessionStatus::Success)
    );
}

// --- Ctrl-C pending-cancel state machine ---

/// Grace window used by the tests below. Long enough to comfortably
/// distinguish "fired at the original deadline" from "reset" under CI
/// scheduling jitter, short enough to keep the suite fast.
const TEST_WINDOW: Duration = Duration::from_millis(200);
/// Extra margin added when waiting for `TEST_WINDOW` to lapse.
const TEST_WINDOW_BUFFER: Duration = Duration::from_millis(150);

fn cli_agent_session(
    status: CLIAgentSessionStatus,
    received_rich_notification: bool,
) -> CLIAgentSession {
    CLIAgentSession {
        agent: CLIAgent::Claude,
        status,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        remote_host: None,
        draft_text: None,
        custom_command_prefix: None,
        received_rich_notification,
    }
}

fn plugin_event(source: CLIAgentEventSource, event: CLIAgentEventType) -> CLIAgentEvent {
    CLIAgentEvent {
        source,
        v: 1,
        agent: CLIAgent::Claude,
        event,
        session_id: None,
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    }
}

fn rich_event(event: CLIAgentEventType) -> CLIAgentEvent {
    plugin_event(CLIAgentEventSource::RichPlugin, event)
}

#[test]
fn ctrl_c_does_not_arm_on_optimistic_in_progress_before_prompt_submit() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });

        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        assert!(
            !model.read(&app, |m, _| m
                .has_pending_or_resolved_ctrl_c_cancel(view_id)),
            "must not arm on the optimistic InProgress set at registration, before any turn started"
        );
    });
}

#[test]
fn ctrl_c_arms_when_in_progress_rich_and_prompt_submitted() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });

        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        assert!(model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));
    });
}

#[test]
fn ctrl_c_arms_when_blocked_rich_and_prompt_submitted() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(
                view_id,
                &rich_event(CLIAgentEventType::PermissionRequest),
                ctx,
            );
        });

        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        assert!(model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));
    });
}

#[test]
fn ctrl_c_does_not_arm_when_status_is_success() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::Stop), ctx);
        });

        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        assert!(!model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));
    });
}

#[test]
fn ctrl_c_does_not_arm_for_codex_osc9_fallback_session() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, false),
                ctx,
            );
        });
        // Codex's OSC 9 fallback never latches `received_rich_notification`,
        // even though `has_seen_prompt_submit` is still tracked.
        model.update(&mut app, |m, ctx| {
            m.update_from_event(
                view_id,
                &plugin_event(
                    CLIAgentEventSource::CodexOsc9Fallback,
                    CLIAgentEventType::PromptSubmit,
                ),
                ctx,
            );
        });

        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        assert!(
            !model.read(&app, |m, _| m
                .has_pending_or_resolved_ctrl_c_cancel(view_id)),
            "a Codex OSC 9 fallback session must never arm the Ctrl-C cancel window"
        );
    });
}

#[test]
fn window_lapse_transitions_session_to_cancelled() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });

        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled)
            );
            assert!(m.has_pending_or_resolved_ctrl_c_cancel(view_id));
        });
    });
}

/// Arms a window for an InProgress+rich session that has seen `prompt_submit`,
/// then applies `disarming_event` immediately and asserts the window no
/// longer resolves to `Cancelled` once it would have lapsed.
fn assert_event_disarms_pending_cancel(disarming_event: CLIAgentEvent) {
    App::test((), |mut app| async move {
        app.add_singleton_model(|ctx| AppExecutionMode::new(ExecutionMode::Sdk, false, ctx));
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        assert!(model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));

        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &disarming_event, ctx);
        });
        assert!(!model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));

        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;

        model.read(&app, |m, _| {
            assert_ne!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled),
                "a disarming event must prevent the lapsed window from cancelling the session"
            );
        });
    });
}

#[test]
fn stop_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::Stop));
}

#[test]
fn stop_failure_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::StopFailure));
}

#[test]
fn permission_request_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::PermissionRequest));
}

#[test]
fn question_asked_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::QuestionAsked));
}

#[test]
fn needs_input_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::NeedsInput));
}

#[test]
fn prompt_submit_event_disarms_pending_cancel() {
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::PromptSubmit));
}

#[test]
fn tool_complete_event_disarms_pending_cancel() {
    // ToolComplete drives a status transition only out of Blocked or a
    // finished turn, but any plugin traffic must still disarm the window.
    assert_event_disarms_pending_cancel(rich_event(CLIAgentEventType::ToolComplete));
}

#[test]
fn idle_prompt_does_not_disarm_pending_cancel() {
    // Unlike other plugin events, `IdlePrompt` means the CLI is sitting
    // idle at its interactive prompt -- evidence of idleness, not
    // aliveness. If it disarmed the window, an idle notification arriving
    // instead of a genuine `stop`/`stop_failure` after an interrupt would
    // leave the session stuck exactly like the bug this feature exists to
    // fix, so the window must survive it and still resolve to `Cancelled`
    // once it lapses.
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        assert!(model.read(&app, |m, _| {
            m.has_pending_or_resolved_ctrl_c_cancel(view_id)
        }));

        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::IdlePrompt), ctx);
        });
        assert!(
            model.read(&app, |m, _| {
                m.has_pending_or_resolved_ctrl_c_cancel(view_id)
            }),
            "an IdlePrompt must not disarm the pending-cancel window"
        );

        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled),
                "an IdlePrompt must not prevent the lapsed window from cancelling the session"
            );
        });
    });
}

#[test]
fn late_stop_after_cancelled_flips_status_to_success() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;
        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled)
            );
        });

        // A `stop` hook that arrives after the window already marked the
        // session Cancelled must still flow through and flip to Success.
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::Stop), ctx);
        });

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Success)
            );
        });
    });
}

#[test]
fn prompt_submit_after_cancelled_returns_session_to_in_progress() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        Timer::after(TEST_WINDOW + TEST_WINDOW_BUFFER).await;
        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled)
            );
        });

        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::InProgress)
            );
        });
    });
}

#[test]
fn stale_timer_callback_after_disarming_event_does_not_overwrite_newer_status() {
    // WarpUI's `SpawnedFutureHandle::abort` only takes effect the next time
    // the future is polled: if the timer already completed and queued its
    // resolve callback before a disarming event arrives, that callback can
    // still run after `abort()` was called on it. The token guard in
    // `resolve_pending_cancel` must make the stale callback a no-op instead
    // of overwriting the disarming event's status.
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        let armed_token = model
            .read(&app, |m, _| {
                m.ctrl_c_cancel_state
                    .get(&view_id)
                    .and_then(|s| s.armed_token)
            })
            .expect("window should be armed");

        // A disarming event arrives before the queued callback runs.
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::Stop), ctx);
        });
        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Success)
            );
        });

        // The stale callback finally runs with the pre-disarm token. It must
        // be a no-op: the disarming event's Success status must survive.
        model.update(&mut app, |m, ctx| {
            m.resolve_pending_cancel(view_id, armed_token, ctx);
        });

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Success),
                "a stale timer callback must not overwrite a newer disarming event's status"
            );
        });
    });
}

#[test]
fn remove_session_clears_pending_cancel_state() {
    // The model is a process-lifetime singleton, so a closed session that
    // saw a `prompt_submit` (and possibly armed a window) must not leave an
    // orphaned `ctrl_c_cancel_state` entry behind.
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, TEST_WINDOW, ctx);
        });
        assert!(model.read(&app, |m, _| m.ctrl_c_cancel_state.contains_key(&view_id)));

        model.update(&mut app, |m, ctx| {
            m.remove_session(view_id, ctx);
        });

        assert!(
            model.read(&app, |m, _| !m.ctrl_c_cancel_state.contains_key(&view_id)),
            "remove_session must clear the ctrl_c_cancel_state entry for the closed session"
        );
    });
}

#[test]
fn second_ctrl_c_while_armed_reuses_the_existing_window() {
    // Deliberately larger/looser than `TEST_WINDOW` so the checkpoint below
    // has a wide, unambiguous margin on both sides: comfortably after the
    // original deadline, and comfortably before what a reset (rather than
    // reused) window would require.
    let window = Duration::from_millis(300);
    let before_deadline = Duration::from_millis(250);
    let checkpoint_after_second_ctrl_c = Duration::from_millis(100);

    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &rich_event(CLIAgentEventType::PromptSubmit), ctx);
        });
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, window, ctx);
        });

        // Second Ctrl-C at t=250ms, shortly before the original t=300ms
        // deadline. Check at t=350ms: 50ms past the original deadline, but
        // 200ms short of a reset deadline (250+300=550ms) — reuse and reset
        // are unambiguous at this checkpoint.
        Timer::after(before_deadline).await;
        model.update(&mut app, |m, ctx| {
            m.observe_ctrl_c_write_with_window(view_id, window, ctx);
        });
        Timer::after(checkpoint_after_second_ctrl_c).await;

        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).map(|s| &s.status),
                Some(&CLIAgentSessionStatus::Cancelled),
                "a second Ctrl-C while armed must reuse the existing window, not reset it"
            );
        });
    });
}

// --- WarpOss: which status changes may notify (and play the sound) ---

/// Replays a pattern seen in a real warp-oss.log: a turn ends while a background
/// subagent runs, the subagent keeps reporting tools, then the turn really ends; plus one
/// permission dialog reported by both the plugin and the permission hook. Only the real end and
/// the first "needs you" may notify.
#[test]
fn only_changes_the_sidebar_shows_are_announced() {
    App::test((), |mut app| async move {
        // Recorded with the background-tasks hook installed.
        let model = app.add_singleton_model(|_| {
            let mut model = CLIAgentSessionsModel::new();
            model.background_hook_seen = true;
            model
        });
        let view_id = EntityId::new();
        let (sender, receiver) = async_channel::unbounded();
        app.update(|ctx| {
            ctx.subscribe_to_model(&model, move |_, event: &CLIAgentSessionsModelEvent, _| {
                if let CLIAgentSessionsModelEvent::StatusChanged {
                    status, announce, ..
                } = event
                {
                    let _ = sender.try_send((status.clone(), *announce));
                }
            });
        });
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });

        let stop = |count: u32| {
            let body = format!(
                r#"{{"v":1,"agent":"claude","event":"stop","background_task_count":{count},"session_cron_count":0}}"#
            );
            parse_event(Some("warp://cli-agent"), &body).unwrap()
        };
        let permission = |summary: &str| {
            let mut event = rich_event(CLIAgentEventType::PermissionRequest);
            event.payload.summary = Some(summary.to_owned());
            event
        };
        let events = [
            rich_event(CLIAgentEventType::PromptSubmit),
            stop(1),
            rich_event(CLIAgentEventType::ToolComplete),
            rich_event(CLIAgentEventType::IdlePrompt),
            stop(0),
            rich_event(CLIAgentEventType::IdlePrompt),
            rich_event(CLIAgentEventType::PromptSubmit),
            permission("Claude needs your permission to use Bash"),
            permission("Claude needs your permission"),
        ];
        for event in &events {
            model.update(&mut app, |m, ctx| m.update_from_event(view_id, event, ctx));
        }

        let mut seen = Vec::new();
        while let Ok(change) = receiver.try_recv() {
            seen.push(change);
        }
        let announced: Vec<bool> = seen.iter().map(|(_, announce)| *announce).collect();
        assert_eq!(
            announced,
            vec![false, false, true, false, true, false],
            "status changes seen: {seen:?}"
        );
    });
}

/// The turn ended with a subagent still running (row: working); when the plugin reports the
/// subagent gone the row turns done, which notifies once.
#[test]
fn background_work_running_out_after_the_turn_is_announced() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        let (sender, receiver) = async_channel::unbounded();
        app.update(|ctx| {
            ctx.subscribe_to_model(&model, move |_, event: &CLIAgentSessionsModelEvent, _| {
                if let CLIAgentSessionsModelEvent::StatusChanged {
                    status, announce, ..
                } = event
                {
                    let _ = sender.try_send((status.clone(), *announce));
                }
            });
        });
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        let stop = parse_event(
            Some("warp://cli-agent"),
            r#"{"v":1,"agent":"claude","event":"stop","background_task_count":1,"session_cron_count":0}"#,
        )
        .unwrap();
        let gone = parse_event(
            Some("warp://cli-agent"),
            r#"{"v":1,"agent":"claude","event":"background_tasks","count":0}"#,
        )
        .unwrap();
        for event in [&stop, &gone] {
            model.update(&mut app, |m, ctx| m.update_from_event(view_id, event, ctx));
        }

        let mut seen = Vec::new();
        while let Ok(change) = receiver.try_recv() {
            seen.push(change);
        }
        assert_eq!(
            seen.last(),
            Some(&(CLIAgentSessionStatus::Success, true)),
            "seen: {seen:?}"
        );
        assert_eq!(
            seen.iter().filter(|(_, announce)| *announce).count(),
            1,
            "seen: {seen:?}"
        );
    });
}

#[test]
fn a_turn_ending_under_running_background_work_is_not_announced() {
    use super::status_change_is_announced as announced;
    let working = CLIAgentSessionStatus::InProgress;
    let done = CLIAgentSessionStatus::Success;
    let needs = CLIAgentSessionStatus::Blocked {
        message: None,
        source: BlockedSource::PermissionRequest,
    };

    // The 48 sounds in the log: a provisional "working" falling back to a finished turn whose
    // background agent still runs. The row says working before and after.
    assert!(!announced(&working, true, &done, true));
    assert!(!announced(&done, true, &done, true));
    // Working -> done and working -> needs you are what the sidebar shows as new.
    assert!(announced(&working, false, &done, false));
    assert!(announced(&working, true, &done, false));
    assert!(announced(&working, false, &needs, false));
    // Background work running out turns the row from working to done, which is news too.
    assert!(announced(&done, true, &done, false));
    // Repeats and Ctrl-C stay silent.
    assert!(!announced(&needs, false, &needs, false));
    assert!(!announced(
        &CLIAgentSessionStatus::Cancelled,
        false,
        &done,
        false
    ));
    assert!(!announced(&done, false, &working, false));
}

/// A replayed Claude session, as the app receives it: the plugin's stop with its raw
/// `background_task_count`, and the background-tasks hook's filtered count.
#[derive(Debug, Clone, Copy)]
enum Replay {
    Prompt,
    Tool,
    Idle,
    /// The hook's count (`running`, monitors left out).
    Hook(u32),
    /// The hook's count with the hook's start time in ms.
    HookAt(u32, u64),
    /// The hook's report with `kinds`: running subagents and shells.
    HookKinds {
        subagents: u32,
        shells: u32,
    },
    /// The plugin's stop with its raw count (every entry, monitors and finished ones included).
    Stop(u32),
    /// Longer than a quiet stop waits for the hook's count.
    Wait,
}

/// Replays `events` with the background-tasks hook installed and checks the row's shown status
/// and how many changes notified.
fn assert_replay(events: &[Replay], working: bool, announced: usize) {
    assert_replay_from(CLIAgentSessionStatus::Success, events, working, announced);
}

fn assert_replay_from(
    start: CLIAgentSessionStatus,
    events: &[Replay],
    working: bool,
    announced: usize,
) {
    replay(start, true, events, working, announced);
}

/// `hook_installed`: an earlier session in this app run already reported through the hook.
/// Returns whether the session ends up listing background shells (the row's dot).
fn replay(
    start: CLIAgentSessionStatus,
    hook_installed: bool,
    events: &[Replay],
    working: bool,
    announced: usize,
) -> bool {
    let events = events.to_vec();
    let shells = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shells_out = shells.clone();
    App::test((), move |mut app| async move {
        let model = app.add_singleton_model(move |_| {
            let mut model = CLIAgentSessionsModel::new();
            model.background_hook_seen = hook_installed;
            model
        });
        let view_id = EntityId::new();
        let (sender, receiver) = async_channel::unbounded();
        app.update(|ctx| {
            ctx.subscribe_to_model(&model, move |_, event: &CLIAgentSessionsModelEvent, _| {
                if let CLIAgentSessionsModelEvent::StatusChanged { announce, .. } = event {
                    let _ = sender.try_send(*announce);
                }
            });
        });
        model.update(&mut app, |m, ctx| {
            m.set_session(view_id, cli_agent_session(start, true), ctx);
        });
        for step in &events {
            let event = match *step {
                Replay::Wait => {
                    Timer::after(super::BACKGROUND_REPORT_WAIT + Duration::from_millis(300)).await;
                    continue;
                }
                Replay::Prompt => rich_event(CLIAgentEventType::PromptSubmit),
                Replay::Tool => rich_event(CLIAgentEventType::ToolComplete),
                Replay::Idle => rich_event(CLIAgentEventType::IdlePrompt),
                Replay::Hook(count) => parse_event(
                    Some("warp://cli-agent"),
                    &format!(r#"{{"v":1,"agent":"claude","event":"background_tasks","count":{count}}}"#),
                )
                .unwrap(),
                Replay::HookKinds { subagents, shells } => parse_event(
                    Some("warp://cli-agent"),
                    &format!(
                        r#"{{"v":1,"agent":"claude","event":"background_tasks","count":{},"kinds":{{"subagent":{subagents},"shell":{shells}}}}}"#,
                        subagents + shells
                    ),
                )
                .unwrap(),
                Replay::HookAt(count, started) => parse_event(
                    Some("warp://cli-agent"),
                    &format!(
                        r#"{{"v":1,"agent":"claude","event":"background_tasks","count":{count},"started_ms":{started}}}"#
                    ),
                )
                .unwrap(),
                Replay::Stop(count) => parse_event(
                    Some("warp://cli-agent"),
                    &format!(
                        r#"{{"v":1,"agent":"claude","event":"stop","background_task_count":{count},"session_cron_count":0}}"#
                    ),
                )
                .unwrap(),
            };
            model.update(&mut app, |m, ctx| m.update_from_event(view_id, &event, ctx));
        }
        let shown_working = model.read(&app, |m, _| {
            let session = m.session(view_id).expect("session");
            shells.store(
                session.session_context.has_background_shells(),
                std::sync::atomic::Ordering::Relaxed,
            );
            session
                .status
                .displayed(session.session_context.running_background_tasks().is_some())
                == CLIAgentSessionStatus::InProgress
        });
        let mut announces = 0;
        while let Ok(announce) = receiver.try_recv() {
            announces += usize::from(announce);
        }
        assert_eq!(shown_working, working, "working after {events:?}");
        assert_eq!(announces, announced, "notifications after {events:?}");
    });
    shells_out.load(std::sync::atomic::Ordering::Relaxed)
}

/// The (plugin count, hook count) pairs of 96 real stops in recorded sessions
/// (WarpOss log). The hook's count always arrived first or in the same second; the plugin's
/// count was higher in 72 of them (monitors, finished agents and shells).
const REAL_STOP_COUNTS: [(u32, u32); 9] = [
    (0, 0),
    (1, 0),
    (1, 1),
    (2, 0),
    (2, 1),
    (2, 2),
    (3, 2),
    (3, 3),
    (4, 3),
];

#[test]
fn real_stops_are_working_exactly_when_the_hook_counts_running_work() {
    use Replay::*;
    for (plugin, hook) in REAL_STOP_COUNTS {
        let running = hook > 0;
        // Hook first (every real stop), then the other order, which the app must survive too.
        assert_replay(
            &[Prompt, Hook(hook), Stop(plugin)],
            running,
            usize::from(!running),
        );
        assert_replay(
            &[Prompt, Stop(plugin), Hook(hook)],
            running,
            usize::from(!running),
        );
        assert_replay(
            &[Prompt, Stop(plugin), Hook(hook), Wait],
            running,
            usize::from(!running),
        );
    }
}

/// A recorded session: subagent stops report one running agent, the main stop's
/// hook says nothing runs, a second later the plugin's stop lists one entry. An earlier version kept
/// that row working with a timer for good.
#[test]
fn a_finished_turn_with_a_stale_plugin_entry_is_done() {
    use Replay::*;
    assert_replay(
        &[
            Prompt,
            Tool,
            Hook(1),
            Hook(1),
            Hook(1),
            Hook(1),
            Hook(0),
            Stop(1),
            Idle,
        ],
        false,
        1,
    );
}

#[test]
fn a_running_monitor_never_keeps_a_finished_turn_working() {
    use Replay::*;
    // The hook leaves monitors out; the plugin counts them at every stop.
    assert_replay(
        &[Prompt, Hook(0), Stop(1), Prompt, Hook(0), Stop(1)],
        false,
        2,
    );
}

#[test]
fn a_hook_count_from_an_older_turn_does_not_carry_over() {
    use Replay::*;
    // Turn 1 ends with a shell running. The turn the shell wakes ends with no hook report: done
    // at once, announced when the wait runs out.
    assert_replay(&[Prompt, Hook(1), Stop(2), Tool, Stop(2)], false, 0);
    assert_replay(&[Prompt, Hook(1), Stop(2), Tool, Stop(2), Wait], false, 1);
    assert_replay(&[Prompt, Hook(1), Stop(2), Stop(2), Wait], false, 1);
    // A late report settles its own stop and does not count for the next turn.
    assert_replay(&[Prompt, Stop(2), Hook(1), Prompt, Stop(2), Wait], false, 1);
    assert_replay(&[Prompt, Stop(2), Hook(1), Tool, Stop(2), Wait], false, 1);
    // With its hook report it ends as that report says.
    assert_replay(
        &[Prompt, Hook(1), Stop(2), Tool, Hook(0), Stop(2)],
        false,
        1,
    );
}

#[test]
fn background_work_running_out_notifies_once() {
    use Replay::*;
    assert_replay(&[Prompt, Hook(1), Stop(2), Hook(0)], false, 1);
    assert_replay(&[Prompt, Stop(2), Hook(1), Hook(0)], false, 1);
}

#[test]
fn a_stop_without_any_hook_report_is_done() {
    use Replay::*;
    assert_replay(&[Prompt, Stop(3)], false, 0);
    assert_replay(&[Prompt, Stop(3), Wait], false, 1);
    assert_replay(&[Prompt, Stop(0)], false, 1);
    assert_replay(&[Prompt, Stop(0), Wait], false, 1);
}

#[test]
fn without_the_hook_a_stop_is_announced_at_once() {
    use Replay::*;
    // No background-tasks hook installed: the plugin's count alone never delays the "done".
    replay(CLIAgentSessionStatus::Success, false, &[Prompt, Stop(3)], false, 1);
    replay(CLIAgentSessionStatus::Success, false, &[Prompt, Stop(0)], false, 1);
    // Once the hook reports, later stops wait for its count as usual.
    replay(
        CLIAgentSessionStatus::Success,
        false,
        &[Prompt, Hook(0), Stop(2), Prompt, Stop(3)],
        false,
        1,
    );
}

#[test]
fn status_neutral_events_keep_a_stop_waiting_for_its_report() {
    use Replay::*;
    assert_replay(&[Prompt, Stop(1), Idle, Hook(0)], false, 1);
    assert_replay(&[Prompt, Stop(1), Idle, Hook(0), Wait], false, 1);
    assert_replay(&[Prompt, Stop(1), Idle, Hook(1)], true, 0);
}

#[test]
fn a_stop_after_a_ctrl_c_stays_silent_when_its_report_settles_it() {
    use Replay::*;
    assert_replay_from(
        CLIAgentSessionStatus::Cancelled,
        &[Stop(1), Hook(0)],
        false,
        0,
    );
    assert_replay_from(CLIAgentSessionStatus::Cancelled, &[Stop(1), Wait], false, 0);
}

/// Found in review: a subagent's hook that started before the main stop's hook but reports after
/// it must not turn the finished tab back to working.
#[test]
fn a_stale_hook_report_does_not_undo_the_turn_end() {
    use Replay::*;
    assert_replay(&[Prompt, Tool, HookAt(0, 300), Stop(1), HookAt(1, 200)], false, 1);
    // In order, the later report wins.
    assert_replay(&[Prompt, Tool, HookAt(0, 200), Stop(1), HookAt(1, 300)], true, 1);
    // Astra: a stale report must not settle a stop that waits for its turn's count.
    assert_replay(
        &[HookAt(0, 300), Prompt, Stop(1), HookAt(0, 200), HookAt(1, 400)],
        true,
        0,
    );
    // A clock set back by more than a slow hook could lag still reports "nothing runs".
    assert_replay(
        &[Prompt, HookAt(1, 100_000), Stop(1), Prompt, Tool, HookAt(0, 50_000), Stop(0)],
        false,
        1,
    );
}

/// WarpOss variant A: background shells never keep a finished turn working; the row shows a
/// normal "done" (announced once, like any finished turn) with a dot while they run. Fails if a
/// shell-only turn still reads as working, a subagent next to a shell reads as done, the dot stays
/// after the shells end, or the shells ending notifies again.
#[test]
fn background_shells_alone_end_the_turn_as_done_with_a_dot() {
    use Replay::*;
    let shells = |subagents, shells| HookKinds { subagents, shells };
    let start = CLIAgentSessionStatus::Success;
    // Turn ends with only a shell running: done, announced once, dot on.
    assert!(replay(
        start.clone(),
        true,
        &[Prompt, shells(0, 1), Stop(1)],
        false,
        1
    ));
    // The shell finishes later: still done, no second notification, dot off.
    assert!(!replay(
        start.clone(),
        true,
        &[Prompt, shells(0, 1), Stop(1), shells(0, 0)],
        false,
        1
    ));
    // The hook's report lands after the stop (a slow transcript scan): the same.
    assert!(replay(
        start.clone(),
        true,
        &[Prompt, Stop(1), shells(0, 1)],
        false,
        1
    ));
    // A subagent next to the shell keeps the row working and quiet.
    replay(
        start.clone(),
        true,
        &[Prompt, shells(1, 1), Stop(2)],
        true,
        0,
    );
    // The subagent finishes, the shell runs on: done, announced once, dot on.
    assert!(replay(
        start.clone(),
        true,
        &[Prompt, shells(1, 1), Stop(2), shells(0, 1)],
        false,
        1
    ));
    // The hook's report before `kinds` existed: all of its work keeps the row working.
    replay(start, true, &[Prompt, Hook(1), Stop(1)], true, 0);
}

#[test]
fn background_tasks_report_carries_its_shell_count() {
    let event = parse_event(
        Some("warp://cli-agent"),
        r#"{"v":1,"agent":"claude","event":"background_tasks","count":3,"kinds":{"subagent":1,"shell":2}}"#,
    )
    .unwrap();
    assert!(matches!(
        event.event,
        CLIAgentEventType::BackgroundTasks {
            count: 3,
            shells: 2,
            only_subagents: false,
            ..
        }
    ));
    let event = parse_event(
        Some("warp://cli-agent"),
        r#"{"v":1,"agent":"claude","event":"background_tasks","count":2}"#,
    )
    .unwrap();
    assert!(matches!(
        event.event,
        CLIAgentEventType::BackgroundTasks { shells: 0, .. }
    ));
}

/// Status changes a test model emitted, as (status, announce).
fn record_status_changes(
    app: &mut App,
    model: &warpui::ModelHandle<CLIAgentSessionsModel>,
) -> async_channel::Receiver<(CLIAgentSessionStatus, bool)> {
    let (sender, receiver) = async_channel::unbounded();
    app.update(|ctx| {
        ctx.subscribe_to_model(model, move |_, event: &CLIAgentSessionsModelEvent, _| {
            if let CLIAgentSessionsModelEvent::StatusChanged {
                status, announce, ..
            } = event
            {
                let _ = sender.try_send((status.clone(), *announce));
            }
        });
    });
    receiver
}

fn drain<T>(receiver: &async_channel::Receiver<T>) -> Vec<T> {
    std::iter::from_fn(|| receiver.try_recv().ok()).collect()
}

fn future_ms() -> u64 {
    u64::try_from(chrono::Local::now().timestamp_millis()).unwrap() + 60_000
}

/// WarpOss: a dialog Claude Code records only in its session file ("waiting") shows "needs you",
/// announced once however many polls see it, and once the file leaves "waiting" the row goes back
/// to what the events said, silently. Fails if the wait is not shown, sticks after the dialog is
/// answered, or notifies on every poll.
#[test]
fn a_session_file_wait_shows_needs_you_once_and_clears_silently() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        let changes = record_status_changes(&mut app, &model);
        let at = future_ms();
        for _ in 0..3 {
            model.update(&mut app, |m, ctx| {
                m.set_external_wait(view_id, Some("permission prompt".to_owned()), at, ctx)
            });
        }
        let blocked = CLIAgentSessionStatus::Blocked {
            message: Some("permission prompt".to_owned()),
            source: BlockedSource::NeedsInput,
        };
        assert_eq!(drain(&changes), vec![(blocked, true)]);

        model.update(&mut app, |m, ctx| {
            m.set_external_wait(view_id, None, 0, ctx)
        });
        model.update(&mut app, |m, ctx| {
            m.set_external_wait(view_id, None, 0, ctx)
        });
        assert_eq!(
            drain(&changes),
            vec![(CLIAgentSessionStatus::InProgress, false)]
        );
        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).unwrap().status,
                CLIAgentSessionStatus::InProgress
            );
        });
    });
}

/// WarpOss: an event that moves the session on (the dialog answered, the turn over) ends the file
/// wait; the file leaving "waiting" afterwards changes nothing. A wait older than the session's
/// last change, or one a hook already shows, is ignored.
#[test]
fn a_session_file_wait_yields_to_events_and_ignores_stale_or_known_waits() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let view_id = EntityId::new();
        model.update(&mut app, |m, ctx| {
            m.set_session(
                view_id,
                cli_agent_session(CLIAgentSessionStatus::InProgress, true),
                ctx,
            );
        });
        let changes = record_status_changes(&mut app, &model);

        // Older than what the app last saw for this session.
        model.update(&mut app, |m, ctx| {
            m.set_external_wait(view_id, Some("x".to_owned()), 1, ctx)
        });
        assert!(drain(&changes).is_empty());

        let at = future_ms();
        model.update(&mut app, |m, ctx| {
            m.set_external_wait(view_id, Some("x".to_owned()), at, ctx)
        });
        let stop = rich_event(CLIAgentEventType::Stop);
        model.update(&mut app, |m, ctx| m.update_from_event(view_id, &stop, ctx));
        drain(&changes);
        model.update(&mut app, |m, ctx| {
            m.set_external_wait(view_id, None, 0, ctx)
        });
        assert!(drain(&changes).is_empty());
        model.read(&app, |m, _| {
            assert_eq!(
                m.session(view_id).unwrap().status,
                CLIAgentSessionStatus::Success
            );
        });

        // A hook-driven "needs you" already shows the dialog: no second announcement.
        let permission = rich_event(CLIAgentEventType::PermissionRequest);
        model.update(&mut app, |m, ctx| {
            m.update_from_event(view_id, &permission, ctx)
        });
        drain(&changes);
        let at = future_ms();
        model.update(&mut app, |m, ctx| {
            m.set_external_wait(view_id, Some("x".to_owned()), at, ctx)
        });
        assert!(drain(&changes).is_empty());
    });
}

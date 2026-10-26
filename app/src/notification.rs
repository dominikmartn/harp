/// At the app level, we have structs for representing the UI framework level
/// notification structs, but with the data parsed to our liking.
/// The similar structs at the UI framework layer are lower-level (mostly strings).
use serde::{Deserialize, Serialize};
use warpui::{EntityId, WindowId};

use crate::pane_group::PaneId;

/// This data is passed along to the MacOS notification delegate and returned
/// to us when the notification is interacted with.
#[derive(Debug, Deserialize, Serialize)]
pub enum NotificationContext {
    /// For block-specific notifications
    BlockOrigin {
        window_id: WindowId,
        pane_group_id: EntityId,
        pane_id: PaneId,
    },
    /// Harp: a Claude Code dialog in a terminal (`claude_waits::notify`); `id` is the
    /// notification's own identifier, which names the dialog it showed.
    ClaudeWait {
        window_id: WindowId,
        pane_group_id: EntityId,
        pane_id: PaneId,
        terminal_view_id: EntityId,
        id: String,
    },
}

/// A click on a `ClaudeWait` notification, for its window's workspace.
#[derive(Debug)]
pub struct ClaudeNoticeClick {
    pub pane_group_id: EntityId,
    pub pane_id: PaneId,
    pub terminal_view_id: EntityId,
    pub id: String,
    /// The action button (`approve-0`, `approve-deny`, `open`); `None` = the notification itself.
    pub action: Option<String>,
}

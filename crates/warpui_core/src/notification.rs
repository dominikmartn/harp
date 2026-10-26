/// At the UI framework level, we have structs for interacting with
/// platform-level notification data (mostly in the form of strings).
/// Similar structs are available at the app level, which are more
/// specific to the data we make use of.
use chrono::NaiveDateTime;
use serde::Serialize;

/// Content to be sent as a notification to the user. Includes `data` that is sent back to
/// application when the notification is clicked--see `NotificationResponse` for more details.  
#[derive(Clone, Debug)]
pub struct UserNotification {
    title: String,
    body: String,
    // Arbitrary data associated with the notification.
    data: Option<String>,
    // Whether to play sound with the notification.
    play_sound: bool,
    /// A line between title and body (macOS).
    subtitle: Option<String>,
    /// The OS identifier, so the notification can be taken back; `None` = one fixed identifier,
    /// a newer notification replaces the older (macOS).
    identifier: Option<String>,
    /// The registered category whose action buttons it shows (macOS).
    category: Option<String>,
}

impl UserNotification {
    /// These limits were discovered experimentally, by testing with example
    /// commands/outputs and ensuring the text was not truncated in most cases.
    /// The official MacOS docs do not mention specific byte/char constraints.
    /// In reality, the strings are limited by the sum of width of the chars,
    /// which is dependent on the string itself (e.g. 'W' is much wider than ' ').
    pub const MAX_TITLE_LENGTH: usize = 40;
    pub const MAX_BODY_LENGTH: usize = 120;

    pub fn new(title: String, body: String, data: Option<String>) -> Self {
        Self {
            title,
            body,
            data,
            play_sound: true,
            subtitle: None,
            identifier: None,
            category: None,
        }
    }

    pub fn new_with_sound(
        title: String,
        body: String,
        data: Option<String>,
        play_sound: bool,
    ) -> Self {
        Self {
            title,
            body,
            data,
            play_sound,
            subtitle: None,
            identifier: None,
            category: None,
        }
    }

    pub fn with_data(mut self, data: impl Into<String>) -> Self {
        self.data = Some(data.into());
        self
    }

    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn with_identifier(mut self, identifier: impl Into<String>) -> Self {
        self.identifier = Some(identifier.into());
        self
    }

    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn subtitle(&self) -> Option<&str> {
        self.subtitle.as_deref()
    }

    pub fn identifier(&self) -> Option<&str> {
        self.identifier.as_deref()
    }

    pub fn category(&self) -> Option<&str> {
        self.category.as_deref()
    }

    pub fn title(&self) -> &str {
        self.title.as_str()
    }

    pub fn body(&self) -> &str {
        self.body.as_str()
    }

    pub fn data(&self) -> Option<&str> {
        self.data.as_deref()
    }

    pub fn play_sound(&self) -> bool {
        self.play_sound
    }
}

/// A response sent when a notification sent by the app was clicked.
#[derive(Debug)]
pub struct NotificationResponse {
    // Time the notification was sent.
    sent_date: NaiveDateTime,

    /// The data associated with the notification, if any. This matches the data included in the
    /// `NotificationContent` when the notification was sent.  
    data: Option<String>,

    /// The action button that was clicked; `None` for a click on the notification itself.
    action: Option<String>,
}

impl NotificationResponse {
    pub fn new(sent_date: NaiveDateTime, data: Option<String>) -> Self {
        NotificationResponse {
            sent_date,
            data,
            action: None,
        }
    }

    pub fn with_action(mut self, action: Option<String>) -> Self {
        self.action = action;
        self
    }

    pub fn action(&self) -> Option<&str> {
        self.action.as_deref()
    }

    pub fn sent_date(&self) -> NaiveDateTime {
        self.sent_date
    }

    pub fn data(&self) -> Option<&str> {
        self.data.as_deref()
    }
}

#[derive(Clone, Debug, Serialize)]
pub enum NotificationSendError {
    /// App does not have permissions to send notifications.
    PermissionsDenied,

    /// On web, there's a difference between permissions being default and being denied. While they are still default,
    /// we should prompt the user to accept or block notifications, since they haven't chosen yet.
    PermissionsNotYetGranted,

    /// Some unknown error occurred when sending the a notification.
    Other { error_message: String },
}

impl NotificationSendError {
    pub fn notifications_error_banner_title(&self) -> &str {
        match self {
            NotificationSendError::PermissionsDenied
            | NotificationSendError::PermissionsNotYetGranted => {
                "Warp tried to send you a notification for the last block but does not have permission."
            }
            NotificationSendError::Other { .. } => {
                "Warp tried to send you a notification for the last block, but something went wrong."
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub enum RequestPermissionsOutcome {
    /// User accepted the request for permissions.
    Accepted,
    /// User explicitly denied permissions.
    PermissionsDenied,
    /// Some unknown error occurred when requesting permissions.
    OtherError { error_message: String },
}

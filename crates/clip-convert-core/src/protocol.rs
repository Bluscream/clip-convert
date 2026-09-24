//! The request and reply exchanged with the dialog process.
//!
//! The application is split in two: a daemon that owns the tray, the hotkey and
//! the clipboard, and a short-lived process that draws one dialog and exits.
//!
//! That split is not incidental. Wayland has no operation for hiding a window —
//! winit's `set_visible` is a documented no-op there — so a long-lived GUI
//! process cannot avoid showing something. Keeping the graphical toolkit in a
//! process that only exists while a dialog is on screen means there is nothing
//! to hide, and nothing held on the graphics driver while the app sits idle in
//! the tray.
//!
//! One JSON object goes in on stdin, one comes back on stdout.

use crate::presets::Preset;
use serde::{Deserialize, Serialize};

/// What the daemon wants shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "dialog", rename_all = "snake_case")]
pub enum Request {
    /// The action menu for the current clipboard content.
    ChooseAction {
        /// Summary line: what is on the clipboard.
        description: String,
        /// `(id, label)` for each action, in config order.
        actions: Vec<(String, String)>,
        /// Remembered state of the "Paste after action" checkbox.
        paste_after: bool,
    },
    /// A character limit.
    AskLimit {
        title: String,
        message: String,
        default: usize,
    },
    /// Which size target to resize an image to.
    AskResizeTarget { presets: Vec<Preset> },
    /// A failure the user needs to see.
    ShowError { message: String },
}

impl Request {
    /// The window title for this dialog.
    #[must_use]
    pub fn title(&self) -> &str {
        match self {
            Self::ChooseAction { .. } => "Clipboard actions",
            Self::AskLimit { title, .. } => title,
            Self::AskResizeTarget { .. } => "Resize image",
            Self::ShowError { .. } => "Something went wrong",
        }
    }
}

/// What the user chose in the action menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionChoice {
    /// The `id` of the chosen action.
    pub action_id: String,
    /// The state of the checkbox when the choice was made.
    pub paste_after: bool,
}

/// What the dialog came back with.
///
/// [`Self::Cancelled`] is an ordinary outcome, not a failure: the user pressed
/// Escape or closed the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    // Struct variants throughout: an internally tagged enum cannot serialise a
    // newtype variant wrapping a primitive.
    Action {
        choice: ActionChoice,
    },
    Limit {
        value: usize,
    },
    ResizeTarget {
        preset: Box<Preset>,
    },
    /// The dialog was shown and acknowledged, with nothing to report.
    Acknowledged,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let encoded = serde_json::to_string(value).expect("encodes");
        serde_json::from_str(&encoded).expect("decodes")
    }

    #[test]
    fn every_request_survives_the_wire() {
        let requests = [
            Request::ChooseAction {
                description: "Text · 99 characters".to_string(),
                actions: vec![("type".to_string(), "Type".to_string())],
                paste_after: true,
            },
            Request::AskLimit {
                title: "Split".to_string(),
                message: "How many?".to_string(),
                default: 2000,
            },
            Request::AskResizeTarget {
                presets: crate::presets::factory(),
            },
            Request::ShowError {
                message: "it broke".to_string(),
            },
        ];
        for request in requests {
            assert_eq!(round_trip(&request), request);
        }
    }

    #[test]
    fn every_reply_survives_the_wire() {
        let replies = [
            Reply::Action {
                choice: ActionChoice {
                    action_id: "type".to_string(),
                    paste_after: false,
                },
            },
            Reply::Limit { value: 140 },
            Reply::ResizeTarget {
                preset: Box::new(crate::presets::factory()[0].clone()),
            },
            Reply::Acknowledged,
            Reply::Cancelled,
        ];
        for reply in replies {
            assert_eq!(round_trip(&reply), reply);
        }
    }

    #[test]
    fn a_request_is_one_line_so_it_can_be_read_with_read_line() {
        let request = Request::AskResizeTarget {
            presets: crate::presets::factory(),
        };
        let encoded = serde_json::to_string(&request).expect("encodes");
        assert!(
            !encoded.contains('\n'),
            "the wire format must stay single-line"
        );
    }

    #[test]
    fn requests_carry_a_tag_so_the_helper_can_dispatch() {
        let encoded = serde_json::to_string(&Request::ShowError {
            message: "x".to_string(),
        })
        .expect("encodes");
        assert!(encoded.contains("\"dialog\":\"show_error\""), "{encoded}");
    }

    #[test]
    fn every_request_has_a_title() {
        assert_eq!(
            Request::ShowError {
                message: "x".to_string()
            }
            .title(),
            "Something went wrong"
        );
        assert_eq!(
            Request::AskLimit {
                title: "Split".to_string(),
                message: String::new(),
                default: 1,
            }
            .title(),
            "Split"
        );
    }

    #[test]
    fn unknown_fields_do_not_break_an_older_helper() {
        // Forward compatibility: a newer daemon may add fields, and the helper
        // should still show the dialog rather than refusing to start.
        let json = r#"{"dialog":"ask_limit","title":"T","message":"M","default":5,"extra":true}"#;
        let decoded: Request = serde_json::from_str(json).expect("tolerates extra fields");
        assert!(matches!(decoded, Request::AskLimit { default: 5, .. }));
    }
}

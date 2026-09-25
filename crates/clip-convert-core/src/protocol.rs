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

/// A window size, in whole logical points.
///
/// Integers rather than floats: this is written to a config file and compared,
/// and a fractional pixel is neither meaningful nor worth the float-equality
/// problems it brings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

impl WindowSize {
    /// Smallest size a dialog may be remembered at.
    ///
    /// Stops a window that was dragged to nothing, or a garbled config value,
    /// from reopening as an unusable sliver.
    pub const MIN_WIDTH: u32 = 320;
    pub const MIN_HEIGHT: u32 = 180;

    /// Whether this is a size worth remembering.
    #[must_use]
    pub fn is_usable(self) -> bool {
        self.width >= Self::MIN_WIDTH && self.height >= Self::MIN_HEIGHT
    }

    /// Builds a size from measured points, rounding to whole ones.
    ///
    /// Returns `None` for anything not worth remembering, including the
    /// non-finite values a compositor can report while a window is being
    /// mapped or destroyed.
    #[must_use]
    pub fn from_points(width: f32, height: f32) -> Option<Self> {
        if !width.is_finite() || !height.is_finite() {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // Guarded above; a negative rounds to 0 and fails the usability check.
        let size = Self {
            width: width.round().max(0.0) as u32,
            height: height.round().max(0.0) as u32,
        };
        size.is_usable().then_some(size)
    }
}

/// What the daemon sends: a dialog to show, and the size to open it at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    pub request: Request,
    /// The size this dialog was last left at, if it has been used before.
    #[serde(default)]
    pub size: Option<WindowSize>,
}

/// What the dialog sends back: the answer, and the size it ended at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub reply: Reply,
    /// The size the window was when it closed, so it can be reopened that way.
    #[serde(default)]
    pub size: Option<WindowSize>,
}

/// One entry in the action menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionEntry {
    pub id: String,
    pub label: String,
    /// Base64 image data drawn to the left of the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Colour of this entry's button, as a hex string such as `#3b5bdb`.
    /// Absent uses the theme's own button colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button_color: Option<String>,
    /// Colour of the text on this entry's button. Absent uses the theme's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
}

/// What the daemon wants shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "dialog", rename_all = "snake_case")]
pub enum Request {
    /// The action menu for the current clipboard content.
    ChooseAction {
        /// Summary line: what is on the clipboard.
        description: String,
        /// The actions to offer, in config order.
        actions: Vec<ActionEntry>,
        /// Remembered state of the "Paste after action" checkbox.
        paste_after: bool,
    },
    /// A character limit.
    AskLimit {
        title: String,
        message: String,
        default: usize,
    },
    /// Which format to convert the content to.
    AskConversion {
        /// What is being converted, for the heading, e.g. `PNG`.
        source: String,
        /// The conversions worth offering, in config order.
        options: Vec<ActionEntry>,
    },
    /// A pattern to search for and what to put in its place.
    AskReplace {
        /// Patterns used before, most recent first, offered for reuse.
        patterns: Vec<String>,
        /// Replacements used before, most recent first.
        replacements: Vec<String>,
    },
    /// Which size target to resize an image to.
    AskResizeTarget { presets: Vec<Preset> },
    /// A failure the user needs to see.
    ShowError { message: String },
}

impl Request {
    /// The key this dialog's remembered size is stored under.
    ///
    /// Per dialog rather than per window: the action menu and the resize
    /// chooser have very different natural shapes, and one remembered size for
    /// both would suit neither.
    #[must_use]
    pub fn size_key(&self) -> &'static str {
        match self {
            Self::ChooseAction { .. } => "actions",
            Self::AskLimit { .. } => "limit",
            Self::AskConversion { .. } => "convert",
            Self::AskReplace { .. } => "replace",
            Self::AskResizeTarget { .. } => "resize",
            Self::ShowError { .. } => "error",
        }
    }

    /// The window title for this dialog.
    #[must_use]
    pub fn title(&self) -> &str {
        match self {
            Self::ChooseAction { .. } => "Clipboard actions",
            Self::AskLimit { title, .. } => title,
            Self::AskConversion { .. } => "Convert to",
            Self::AskReplace { .. } => "Find and replace",
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
    Replace {
        replacement: crate::replace::Replacement,
    },
    /// The `id` of the chosen conversion.
    Conversion {
        conversion_id: String,
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
                actions: vec![ActionEntry {
                    id: "type".to_string(),
                    label: "Type".to_string(),
                    icon: None,
                    button_color: Some("#3b5bdb".to_string()),
                    text_color: None,
                }],
                paste_after: true,
            },
            Request::AskLimit {
                title: "Split".to_string(),
                message: "How many?".to_string(),
                default: 2000,
            },
            Request::AskConversion {
                source: "PNG".to_string(),
                options: vec![ActionEntry {
                    id: "to-ico".to_string(),
                    label: "To ICO".to_string(),
                    icon: None,
                    button_color: None,
                    text_color: None,
                }],
            },
            Request::AskReplace {
                patterns: vec![r"\d+".to_string()],
                replacements: vec!["$1".to_string()],
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
            Reply::Replace {
                replacement: crate::replace::Replacement {
                    pattern: r"(\w+), (\w+)".to_string(),
                    replacement: "$2 $1".to_string(),
                },
            },
            Reply::Conversion {
                conversion_id: "to-ico".to_string(),
            },
            Reply::Acknowledged,
            Reply::Cancelled,
        ];
        for reply in replies {
            assert_eq!(round_trip(&reply), reply);
        }
    }

    #[test]
    fn an_ask_and_answer_survive_the_wire() {
        let ask = Ask {
            request: Request::AskLimit {
                title: "Split".to_string(),
                message: "How many?".to_string(),
                default: 2000,
            },
            size: Some(WindowSize {
                width: 500,
                height: 300,
            }),
        };
        assert_eq!(round_trip(&ask), ask);

        let answer = Answer {
            reply: Reply::Limit { value: 12 },
            size: None,
        };
        assert_eq!(round_trip(&answer), answer);
    }

    #[test]
    fn an_ask_without_a_size_still_decodes() {
        // The first time a dialog is used there is nothing remembered.
        let json = r#"{"request":{"dialog":"show_error","message":"x"}}"#;
        let ask: Ask = serde_json::from_str(json).expect("size is optional");
        assert_eq!(ask.size, None);
    }

    #[test]
    fn every_dialog_has_its_own_size_key() {
        let keys = [
            Request::ChooseAction {
                description: String::new(),
                actions: Vec::new(),
                paste_after: false,
            }
            .size_key(),
            Request::AskLimit {
                title: String::new(),
                message: String::new(),
                default: 1,
            }
            .size_key(),
            Request::AskConversion {
                source: String::new(),
                options: Vec::new(),
            }
            .size_key(),
            Request::AskReplace {
                patterns: Vec::new(),
                replacements: Vec::new(),
            }
            .size_key(),
            Request::AskResizeTarget {
                presets: Vec::new(),
            }
            .size_key(),
            Request::ShowError {
                message: String::new(),
            }
            .size_key(),
        ];
        let unique: std::collections::BTreeSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len(), "size keys must not collide");
    }

    #[test]
    fn an_unusable_size_is_rejected() {
        // A window dragged to nothing, or a garbled config value, must not be
        // remembered — it would reopen unusable.
        assert!(!WindowSize {
            width: 10,
            height: 10
        }
        .is_usable());
        assert!(WindowSize {
            width: 460,
            height: 400
        }
        .is_usable());

        // Measurements a compositor can report mid-map must not be stored.
        assert_eq!(WindowSize::from_points(f32::NAN, 400.0), None);
        assert_eq!(WindowSize::from_points(500.0, f32::INFINITY), None);
        assert_eq!(WindowSize::from_points(-10.0, 400.0), None);
        assert_eq!(WindowSize::from_points(10.0, 10.0), None);
        assert_eq!(
            WindowSize::from_points(460.4, 399.6),
            Some(WindowSize {
                width: 460,
                height: 400
            })
        );
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

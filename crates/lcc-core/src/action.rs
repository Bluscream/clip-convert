//! The action model: what the menu offers, and what running an entry means.
//!
//! An action is either one of the [`Builtin`] behaviours compiled into the app or
//! an external command described entirely in the config file. Adding, removing,
//! reordering or re-labelling actions therefore never requires a rebuild.
//!
//! Config is parsed into [`ActionSpec`] and then validated into [`Action`], so
//! that a bad `when` list or a builtin with no matching name is reported once at
//! load time rather than discovered when the user clicks the entry.

use crate::content::ContentKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A behaviour implemented inside the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Builtin {
    /// Emulate typing the text into the focused window.
    Type,
    /// Replace a URL with a shortened one.
    Shorten,
    /// Send the text in fixed-size pieces, one after another.
    Split,
    /// Shorten the text to fit a limit, marking the cut.
    Truncate,
    /// Re-encode an image to fit dimension and file-size limits.
    Resize,
}

/// How clipboard content reaches an external command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    /// Written to the command's standard input. The default, and the only mode
    /// that handles binary image data and arbitrarily long text safely.
    #[default]
    Stdin,
    /// Appended to the command as a final argument. Text only.
    Argument,
    /// Written to a temporary file whose path is appended as a final argument.
    File,
    /// Not passed at all; the command receives only its configured arguments.
    None,
}

/// What to do with an external command's standard output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// Put it back on the clipboard.
    #[default]
    Clipboard,
    /// Type it into the focused window.
    Type,
    /// Show it as a desktop notification.
    Notify,
    /// Ignore it; the command did its own work.
    Discard,
}

/// One `[[actions]]` entry exactly as written in the config file.
///
/// `deny_unknown_fields` makes a misspelled key a load error rather than a
/// setting that silently does nothing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSpec {
    /// Stable identifier, used in logs and to refer to the action internally.
    pub id: String,
    /// The text shown on the button.
    pub label: String,
    /// Content kinds this action applies to. Empty, or containing `any`, means
    /// every kind.
    #[serde(default)]
    pub when: Vec<String>,
    /// Names a compiled-in behaviour. Mutually exclusive with `command`.
    #[serde(default)]
    pub builtin: Option<Builtin>,
    /// The program and arguments to run. Mutually exclusive with `builtin`.
    ///
    /// This is an argument vector, never a shell string: clipboard content is
    /// untrusted input and must not reach a shell.
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub input: InputMode,
    #[serde(default)]
    pub output: OutputMode,
    /// Set false to keep an entry in the file but hide it from the menu.
    #[serde(default = "crate::config::default_true")]
    pub enabled: bool,
}

/// What running an action actually does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    Builtin(Builtin),
    Command {
        argv: Vec<String>,
        input: InputMode,
        output: OutputMode,
    },
}

/// A validated action, ready to be offered in the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub id: String,
    pub label: String,
    pub kinds: BTreeSet<ContentKind>,
    pub run: Run,
}

/// Why an `[[actions]]` entry could not be used.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ActionError {
    #[error("action `{id}` sets both `builtin` and `command`; it must set exactly one")]
    BothBuiltinAndCommand { id: String },
    #[error("action `{id}` sets neither `builtin` nor `command`; it must set exactly one")]
    NeitherBuiltinNorCommand { id: String },
    #[error("action `{id}` has an empty `command`")]
    EmptyCommand { id: String },
    #[error("action `{id}` has an empty `id` or `label`")]
    Unnamed { id: String },
    #[error("action `{id}` has unknown content kind `{kind}` in `when` (valid: url, text, image, any)")]
    UnknownKind { id: String, kind: String },
    #[error("duplicate action id `{id}`")]
    DuplicateId { id: String },
}

impl Action {
    /// Validates one config entry.
    pub fn from_spec(spec: &ActionSpec) -> Result<Self, ActionError> {
        let id = spec.id.trim().to_string();
        if id.is_empty() || spec.label.trim().is_empty() {
            return Err(ActionError::Unnamed { id: spec.id.clone() });
        }

        let has_command = !spec.command.is_empty();
        let run = match (spec.builtin, has_command) {
            (Some(_), true) => return Err(ActionError::BothBuiltinAndCommand { id }),
            (None, false) => return Err(ActionError::NeitherBuiltinNorCommand { id }),
            (Some(builtin), false) => Run::Builtin(builtin),
            (None, true) => {
                if spec.command.iter().all(|part| part.trim().is_empty()) {
                    return Err(ActionError::EmptyCommand { id });
                }
                Run::Command {
                    argv: spec.command.clone(),
                    input: spec.input,
                    output: spec.output,
                }
            }
        };

        Ok(Self {
            kinds: parse_when(&id, &spec.when)?,
            id,
            label: spec.label.trim().to_string(),
            run,
        })
    }

    /// Whether this action should appear for the given content.
    #[must_use]
    pub fn applies_to(&self, kind: ContentKind) -> bool {
        self.kinds.contains(&kind)
    }
}

/// Turns a `when` list into a set of kinds. Empty or `any` means all kinds.
fn parse_when(id: &str, when: &[String]) -> Result<BTreeSet<ContentKind>, ActionError> {
    if when.is_empty() || when.iter().any(|w| w.trim().eq_ignore_ascii_case("any")) {
        return Ok(ContentKind::all().into_iter().collect());
    }

    when.iter()
        .map(|name| {
            ContentKind::parse(name).ok_or_else(|| ActionError::UnknownKind {
                id: id.to_string(),
                kind: name.clone(),
            })
        })
        .collect()
}

/// Validates a whole list, rejecting duplicate ids.
///
/// Duplicates are an error rather than last-one-wins because two entries sharing
/// an id is almost always a copy-paste mistake, and silently dropping one of them
/// looks exactly like the action not working.
pub fn validate_all(specs: &[ActionSpec]) -> Result<Vec<Action>, ActionError> {
    let mut seen = BTreeSet::new();
    let mut actions = Vec::with_capacity(specs.len());

    for spec in specs.iter().filter(|s| s.enabled) {
        let action = Action::from_spec(spec)?;
        if !seen.insert(action.id.clone()) {
            return Err(ActionError::DuplicateId { id: action.id });
        }
        actions.push(action);
    }

    Ok(actions)
}

/// The actions to offer for the given content, in config order.
#[must_use]
pub fn for_kind(actions: &[Action], kind: ContentKind) -> Vec<&Action> {
    actions.iter().filter(|a| a.applies_to(kind)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str) -> ActionSpec {
        ActionSpec {
            id: id.to_string(),
            label: id.to_string(),
            when: Vec::new(),
            builtin: Some(Builtin::Type),
            command: Vec::new(),
            input: InputMode::default(),
            output: OutputMode::default(),
            enabled: true,
        }
    }

    #[test]
    fn an_empty_when_list_means_every_kind() {
        let action = Action::from_spec(&spec("a")).expect("valid");
        for kind in ContentKind::all() {
            assert!(action.applies_to(kind));
        }
    }

    #[test]
    fn any_means_every_kind() {
        let mut s = spec("a");
        s.when = vec!["any".to_string()];
        let action = Action::from_spec(&s).expect("valid");
        assert_eq!(action.kinds.len(), 3);
    }

    #[test]
    fn when_restricts_the_action() {
        let mut s = spec("a");
        s.when = vec!["url".to_string()];
        let action = Action::from_spec(&s).expect("valid");
        assert!(action.applies_to(ContentKind::Url));
        assert!(!action.applies_to(ContentKind::Text));
        assert!(!action.applies_to(ContentKind::Image));
    }

    #[test]
    fn an_unknown_kind_is_rejected_by_name() {
        let mut s = spec("a");
        s.when = vec!["video".to_string()];
        assert_eq!(
            Action::from_spec(&s),
            Err(ActionError::UnknownKind {
                id: "a".to_string(),
                kind: "video".to_string()
            })
        );
    }

    #[test]
    fn builtin_and_command_are_mutually_exclusive() {
        let mut s = spec("a");
        s.command = vec!["echo".to_string()];
        assert_eq!(
            Action::from_spec(&s),
            Err(ActionError::BothBuiltinAndCommand { id: "a".to_string() })
        );

        s.builtin = None;
        assert!(Action::from_spec(&s).is_ok());

        s.command = Vec::new();
        assert_eq!(
            Action::from_spec(&s),
            Err(ActionError::NeitherBuiltinNorCommand { id: "a".to_string() })
        );
    }

    #[test]
    fn a_blank_command_is_rejected() {
        let mut s = spec("a");
        s.builtin = None;
        s.command = vec!["  ".to_string()];
        assert_eq!(
            Action::from_spec(&s),
            Err(ActionError::EmptyCommand { id: "a".to_string() })
        );
    }

    #[test]
    fn a_blank_label_is_rejected() {
        let mut s = spec("a");
        s.label = "   ".to_string();
        assert!(matches!(
            Action::from_spec(&s),
            Err(ActionError::Unnamed { .. })
        ));
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let specs = vec![spec("a"), spec("a")];
        assert_eq!(
            validate_all(&specs),
            Err(ActionError::DuplicateId { id: "a".to_string() })
        );
    }

    #[test]
    fn disabled_actions_are_dropped_and_do_not_trip_duplicate_detection() {
        let mut off = spec("a");
        off.enabled = false;
        let actions = validate_all(&[off, spec("a")]).expect("valid");
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn for_kind_preserves_config_order() {
        let mut url_only = spec("second");
        url_only.when = vec!["url".to_string()];
        let actions = validate_all(&[spec("first"), url_only, spec("third")]).expect("valid");

        let offered: Vec<&str> = for_kind(&actions, ContentKind::Url)
            .iter()
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(offered, ["first", "second", "third"]);

        let offered: Vec<&str> = for_kind(&actions, ContentKind::Text)
            .iter()
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(offered, ["first", "third"]);
    }

    #[test]
    fn an_unknown_key_in_the_config_is_an_error() {
        let bad = r#"id = "a"
label = "A"
builtin = "type"
whne = ["url"]
"#;
        let err = toml::from_str::<ActionSpec>(bad).expect_err("typo should be rejected");
        assert!(err.to_string().contains("whne"), "{err}");
    }

    #[test]
    fn an_unknown_builtin_names_the_valid_ones() {
        let bad = r#"id = "a"
label = "A"
builtin = "frobnicate"
"#;
        let err = toml::from_str::<ActionSpec>(bad).expect_err("unknown builtin");
        assert!(err.to_string().contains("type"), "{err}");
    }
}

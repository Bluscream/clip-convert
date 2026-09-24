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
    /// A small picture shown on this action's button.
    ///
    /// May be written as base64, a `data:` URI, an `http(s)` URL or a local
    /// path; anything not already base64 is converted once and written back
    /// here, so it is never fetched again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl Builtin {
    /// The kind this behaviour actually operates on.
    ///
    /// Distinct from the `when` list, which only decides whether the action is
    /// offered. `Type` is offered for a file selection but works on its text
    /// form, so it should read "Type Text" rather than "Type Files".
    #[must_use]
    pub fn consumes(self) -> ContentKind {
        match self {
            Self::Type | Self::Split | Self::Truncate => ContentKind::Text,
            Self::Shorten => ContentKind::Url,
            Self::Resize => ContentKind::Image,
        }
    }
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
    /// Base64 image data for the button, if one was configured.
    pub icon: Option<String>,
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
    #[error(
        "action `{id}` has unknown content kind `{kind}` in `when` (valid: {}, any)",
        crate::content::ContentKind::all()
            .map(crate::content::ContentKind::as_str)
            .join(", ")
    )]
    UnknownKind { id: String, kind: String },
    #[error("duplicate action id `{id}`")]
    DuplicateId { id: String },
}

impl Action {
    /// Validates one config entry.
    ///
    /// # Errors
    ///
    /// Returns the specific [`ActionError`] describing what is wrong with the
    /// entry, so the message can name the offending action by id.
    pub fn from_spec(spec: &ActionSpec) -> Result<Self, ActionError> {
        let id = spec.id.trim().to_string();
        if id.is_empty() || spec.label.trim().is_empty() {
            return Err(ActionError::Unnamed {
                id: spec.id.clone(),
            });
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
            icon: spec.icon.clone(),
        })
    }

    /// The button text for this action against particular content.
    ///
    /// The configured label names the verb; the noun comes from what the action
    /// will operate on, so one entry reads correctly for one image and for
    /// several. A label that already names its subject is left alone, so a
    /// custom action called "Copy file names" does not become
    /// "Copy file names Files".
    #[must_use]
    pub fn display_label(&self, clip: &crate::content::Clip) -> String {
        let Some(kind) = self.target_kind(clip) else {
            return self.label.clone();
        };
        let noun = clip.noun_for(kind);

        let label = self.label.to_ascii_lowercase();
        let singular = noun.trim_end_matches('s').to_ascii_lowercase();
        if label.contains(&singular) {
            return self.label.clone();
        }

        format!("{} {noun}", self.label)
    }

    /// The kind this action will operate on for the given content.
    fn target_kind(&self, clip: &crate::content::Clip) -> Option<ContentKind> {
        let available = clip.kinds();

        if let Run::Builtin(builtin) = self.run {
            let consumed = builtin.consumes();
            // No noun when the thing it works on is not there. "Type" offered
            // for a bare image would otherwise read "Type Image", which is
            // exactly the one thing it cannot do.
            return available.contains(&consumed).then_some(consumed);
        }

        // A configured command says nothing about what it reads, so the most
        // specific kind it was offered for is the best available guess.
        self.kinds.intersection(&available).next().copied()
    }

    /// Whether this action should appear for content offering `available`.
    ///
    /// A clipboard offers several kinds at once — a file selection is also
    /// text, and a copied image may carry its source URL — so an action is
    /// offered when *any* of them matches.
    #[must_use]
    pub fn applies_to(&self, available: &BTreeSet<ContentKind>) -> bool {
        !self.kinds.is_disjoint(available)
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
///
/// # Errors
///
/// Returns the first [`ActionError`] found, including a duplicate id.
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

/// The actions to offer for content of the given kinds, in config order.
#[must_use]
pub fn for_kinds<'a>(actions: &'a [Action], available: &BTreeSet<ContentKind>) -> Vec<&'a Action> {
    actions.iter().filter(|a| a.applies_to(available)).collect()
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
            icon: None,
        }
    }

    use crate::content::Clip;
    use std::path::PathBuf;

    fn factory() -> Vec<Action> {
        validate_all(&crate::config::factory_actions()).expect("factory actions are valid")
    }

    fn labels_for(clip: &Clip) -> Vec<String> {
        let actions = factory();
        for_kinds(&actions, &clip.kinds())
            .iter()
            .map(|a| a.display_label(clip))
            .collect()
    }

    #[test]
    fn labels_name_what_the_action_will_act_on() {
        let clip = Clip::from_text("some words").expect("non-empty");
        assert_eq!(
            labels_for(&clip),
            ["Type Text", "Split Text", "Truncate Text"]
        );
    }

    #[test]
    fn a_url_reads_as_a_url_but_text_actions_still_say_text() {
        let clip = Clip::from_text("https://example.com/a").expect("non-empty");
        assert_eq!(
            labels_for(&clip),
            ["Type Text", "Shorten URL", "Split Text", "Truncate Text"]
        );
    }

    #[test]
    fn a_single_image_is_singular() {
        let clip = Clip::from_image("image/png".to_string(), vec![1, 2, 3]).expect("non-empty");
        // "Type" keeps its bare label: an image has no text form, so there is
        // no honest noun to add.
        assert_eq!(labels_for(&clip), ["Type", "Resize Image"]);
    }

    #[test]
    fn several_image_files_are_plural() {
        let clip = Clip::from_files(vec![
            PathBuf::from("/a/one.png"),
            PathBuf::from("/a/two.png"),
        ])
        .expect("non-empty");
        assert!(
            labels_for(&clip).contains(&"Resize Images".to_string()),
            "{:?}",
            labels_for(&clip)
        );
    }

    #[test]
    fn one_image_file_is_singular() {
        let clip = Clip::from_files(vec![PathBuf::from("/a/one.png")]).expect("non-empty");
        assert!(
            labels_for(&clip).contains(&"Resize Image".to_string()),
            "{:?}",
            labels_for(&clip)
        );
    }

    #[test]
    fn typing_a_file_selection_still_reads_as_text() {
        // Offered because of the synthesised text facet, so the noun must come
        // from what it consumes rather than from the most specific kind present.
        let clip = Clip::from_files(vec![PathBuf::from("/a/one.png")]).expect("non-empty");
        assert!(labels_for(&clip).contains(&"Type Text".to_string()));
    }

    #[test]
    fn a_label_that_already_names_its_subject_is_left_alone() {
        let mut spec = spec("custom");
        spec.label = "Copy file names".to_string();
        spec.when = vec!["files".to_string()];
        spec.builtin = None;
        spec.command = vec!["true".to_string()];
        let action = Action::from_spec(&spec).expect("valid");

        let clip = Clip::from_files(vec![PathBuf::from("/a/one.png")]).expect("non-empty");
        assert_eq!(action.display_label(&clip), "Copy file names");
    }

    #[test]
    fn a_command_action_takes_the_most_specific_kind_it_was_offered_for() {
        let mut spec = spec("convert");
        spec.label = "Convert".to_string();
        spec.when = vec!["files".to_string(), "text".to_string()];
        spec.builtin = None;
        spec.command = vec!["true".to_string()];
        let action = Action::from_spec(&spec).expect("valid");

        let clip = Clip::from_files(vec![
            PathBuf::from("/a/one.png"),
            PathBuf::from("/a/two.png"),
        ])
        .expect("non-empty");
        assert_eq!(action.display_label(&clip), "Convert Files");
    }

    #[test]
    fn an_empty_when_list_means_every_kind() {
        let action = Action::from_spec(&spec("a")).expect("valid");
        for kind in ContentKind::all() {
            assert!(action.applies_to(&BTreeSet::from([kind])));
        }
    }

    #[test]
    fn any_means_every_kind() {
        let mut s = spec("a");
        s.when = vec!["any".to_string()];
        let action = Action::from_spec(&s).expect("valid");
        assert_eq!(action.kinds.len(), ContentKind::all().len());
    }

    #[test]
    fn when_restricts_the_action() {
        let mut s = spec("a");
        s.when = vec!["url".to_string()];
        let action = Action::from_spec(&s).expect("valid");
        assert!(action.applies_to(&BTreeSet::from([ContentKind::Url])));
        assert!(!action.applies_to(&BTreeSet::from([ContentKind::Text])));
        assert!(!action.applies_to(&BTreeSet::from([ContentKind::Image])));
    }

    #[test]
    fn an_unknown_kind_is_rejected_by_name() {
        let mut s = spec("a");
        s.when = vec!["audio".to_string()];
        assert_eq!(
            Action::from_spec(&s),
            Err(ActionError::UnknownKind {
                id: "a".to_string(),
                kind: "audio".to_string()
            })
        );
        // The message must name the kinds that would have worked.
        let message = Action::from_spec(&s).expect_err("unknown").to_string();
        assert!(message.contains("files"), "{message}");
        assert!(message.contains("video"), "{message}");
    }

    #[test]
    fn builtin_and_command_are_mutually_exclusive() {
        let mut s = spec("a");
        s.command = vec!["echo".to_string()];
        assert_eq!(
            Action::from_spec(&s),
            Err(ActionError::BothBuiltinAndCommand {
                id: "a".to_string()
            })
        );

        s.builtin = None;
        assert!(Action::from_spec(&s).is_ok());

        s.command = Vec::new();
        assert_eq!(
            Action::from_spec(&s),
            Err(ActionError::NeitherBuiltinNorCommand {
                id: "a".to_string()
            })
        );
    }

    #[test]
    fn a_blank_command_is_rejected() {
        let mut s = spec("a");
        s.builtin = None;
        s.command = vec!["  ".to_string()];
        assert_eq!(
            Action::from_spec(&s),
            Err(ActionError::EmptyCommand {
                id: "a".to_string()
            })
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
            Err(ActionError::DuplicateId {
                id: "a".to_string()
            })
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

        let offered: Vec<&str> = for_kinds(&actions, &BTreeSet::from([ContentKind::Url]))
            .iter()
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(offered, ["first", "second", "third"]);

        let offered: Vec<&str> = for_kinds(&actions, &BTreeSet::from([ContentKind::Text]))
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

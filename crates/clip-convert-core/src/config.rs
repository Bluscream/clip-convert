//! Configuration: every tunable the app has, and where it is read from.
//!
//! The guiding rule is that nothing behavioural is compiled in. Action lists,
//! their labels and order, the size presets, and even the external programs used
//! to type text and convert images all come from the file, so the app can be
//! re-tasked by editing TOML.
//!
//! `deny_unknown_fields` is set throughout: a misspelled key is reported at
//! startup instead of silently doing nothing, which is the failure mode that
//! makes a config-driven app frustrating.

use crate::action::{Action, ActionSpec, Builtin, InputMode, OutputMode};
use crate::presets::{self, Preset};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub(crate) const fn default_true() -> bool {
    true
}

/// Optional overrides for the built-in input backend.
///
/// Everything is native and cross-platform by default, so all of these are
/// `None` and the config file does not need a `[commands]` section at all.
///
/// They exist for Wayland, where synthesising input from an ordinary client is
/// forbidden and an external helper such as `ydotool` is the only route, and for
/// anyone who wants to substitute their own tool.
///
/// These are argument vectors rather than shell strings, so clipboard content
/// never reaches a shell. `{delay}` is substituted with the per-key delay.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Commands {
    /// Types text read from standard input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_text: Option<Vec<String>>,
    /// Presses Return, used between the pieces of a split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_enter: Option<Vec<String>>,
    /// Presses the paste chord, used by "Paste after action".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_paste: Option<Vec<String>>,
}

/// Defaults for the `split` built-in.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SplitSettings {
    /// Pre-filled in the prompt.
    pub default_limit: usize,
    /// Pause between pieces, giving the target app time to accept each one.
    pub delay_ms: u64,
    /// Press Return after each piece.
    pub press_enter: bool,
}

impl Default for SplitSettings {
    fn default() -> Self {
        Self {
            default_limit: 2000,
            delay_ms: 1000,
            press_enter: true,
        }
    }
}

/// Defaults for the `truncate` built-in.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TruncateSettings {
    pub default_limit: usize,
    /// Marker appended to show the text was cut. Counts toward the limit.
    pub ellipsis: String,
}

impl Default for TruncateSettings {
    fn default() -> Self {
        Self {
            default_limit: 2000,
            ellipsis: "...".to_string(),
        }
    }
}

/// Defaults and remembered history for the `replace` built-in.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceSettings {
    /// How many past patterns and replacements to offer. Zero remembers none.
    #[serde(default = "default_history")]
    pub history_limit: usize,
    /// Patterns used before, most recent first. Written by the app, but
    /// perfectly reasonable to seed by hand with the ones worth keeping.
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Replacements used before, most recent first.
    #[serde(default)]
    pub replacements: Vec<String>,
}

const fn default_history() -> usize {
    10
}

impl Default for ReplaceSettings {
    fn default() -> Self {
        Self {
            history_limit: default_history(),
            patterns: Vec::new(),
            replacements: Vec::new(),
        }
    }
}

/// How a shortener is talked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortenerKind {
    /// A YOURLS instance driven through `yourls-api.php`.
    Yourls,
    /// Any program that reads a URL on stdin and prints the short URL.
    Command,
}

/// One shortening backend. `Shorten` picks at random among the enabled ones.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Shortener {
    pub name: String,
    pub kind: ShortenerKind,
    #[serde(default)]
    pub api_url: String,
    #[serde(default)]
    pub signature: String,
    /// Public prefix of links this instance serves. Used to avoid re-shortening
    /// a link that is already short.
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// The whole configuration.
// Each flag is an independent user-facing toggle with its own config key;
// grouping them into sub-structs purely to satisfy the lint would change the
// file format without making anything clearer.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Shorten URLs automatically when they are copied. Toggled from the tray.
    #[serde(default = "default_true")]
    pub auto_shorten: bool,
    /// Remembered state of the action dialog's checkbox.
    #[serde(default = "default_true")]
    pub paste_after_action: bool,
    /// Chord that opens the action menu, e.g. `ctrl+b`.
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default = "default_true")]
    pub notifications: bool,
    /// URLs matching this are never auto-shortened. Empty disables the check.
    #[serde(default)]
    pub blacklist_regex: String,
    /// Hold Shift while copying to skip auto-shortening once.
    #[serde(default = "default_true")]
    pub bypass_shift: bool,
    /// Skip auto-shortening entirely while Scroll Lock is on.
    #[serde(default = "default_true")]
    pub bypass_scroll_lock: bool,
    /// Copying the same URL twice in a row leaves it alone.
    #[serde(default = "default_true")]
    pub bypass_double_copy: bool,
    /// Accept invalid TLS certificates when talking to a shortener. Only for a
    /// self-hosted instance with a self-signed certificate.
    #[serde(default)]
    pub ignore_ssl_errors: bool,
    /// Per-key delay for typing, in ms. Higher is slower but more reliable in
    /// apps that drop synthesised keys.
    #[serde(default = "default_type_delay")]
    pub type_delay_ms: u32,
    /// Pause after the action dialog closes, before typing or pasting, giving
    /// the compositor time to return focus to the window the user was in.
    #[serde(default = "default_focus_delay")]
    pub focus_restore_delay_ms: u64,
    #[serde(default)]
    pub split: SplitSettings,
    #[serde(default)]
    pub truncate: TruncateSettings,
    #[serde(default)]
    pub replace: ReplaceSettings,
    #[serde(default)]
    pub commands: Commands,
    #[serde(default)]
    pub shorteners: Vec<Shortener>,
    #[serde(default = "presets::factory")]
    pub presets: Vec<Preset>,
    /// What the `convert` built-in can turn things into.
    #[serde(default = "crate::convert::factory")]
    pub conversions: Vec<crate::convert::Conversion>,
    #[serde(default = "factory_actions")]
    pub actions: Vec<ActionSpec>,
    /// Sizes the dialogs were last left at, keyed by dialog.
    ///
    /// Written by the app rather than by hand; editing it is harmless but
    /// pointless, since the next resize overwrites it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub window_sizes: BTreeMap<String, crate::protocol::WindowSize>,
}

fn default_hotkey() -> String {
    "ctrl+b".to_string()
}

const fn default_type_delay() -> u32 {
    12
}

const fn default_focus_delay() -> u64 {
    250
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_shorten: true,
            paste_after_action: true,
            hotkey: default_hotkey(),
            notifications: true,
            blacklist_regex: String::new(),
            bypass_shift: true,
            bypass_scroll_lock: true,
            bypass_double_copy: true,
            ignore_ssl_errors: false,
            type_delay_ms: default_type_delay(),
            focus_restore_delay_ms: default_focus_delay(),
            split: SplitSettings::default(),
            truncate: TruncateSettings::default(),
            replace: ReplaceSettings::default(),
            commands: Commands::default(),
            shorteners: Vec::new(),
            presets: presets::factory(),
            conversions: crate::convert::factory(),
            actions: factory_actions(),
            window_sizes: BTreeMap::new(),
        }
    }
}

/// The action list written into a fresh config file.
#[must_use]
pub fn factory_actions() -> Vec<ActionSpec> {
    let builtin = |id: &str, label: &str, when: &[&str], which: Builtin| ActionSpec {
        id: id.to_string(),
        label: label.to_string(),
        when: when.iter().map(|s| (*s).to_string()).collect(),
        builtin: Some(which),
        command: Vec::new(),
        input: InputMode::default(),
        output: OutputMode::default(),
        enabled: true,
        icon: None,
        button_color: None,
        text_color: None,
    };

    vec![
        // Offered for every kind: with an image on the clipboard it reports that
        // there is nothing to type rather than being silently absent.
        builtin("type", "Type", &["any"], Builtin::Type),
        builtin("shorten", "Shorten", &["url"], Builtin::Shorten),
        builtin("split", "Split", &["text", "url"], Builtin::Split),
        builtin("truncate", "Truncate", &["text", "url"], Builtin::Truncate),
        builtin(
            "replace",
            "Replace in",
            &["text", "url", "html"],
            Builtin::Replace,
        ),
        builtin("resize", "Resize", &["image"], Builtin::Resize),
        builtin(
            "convert",
            "Convert",
            &["image", "files", "html"],
            Builtin::Convert,
        ),
    ]
}

/// Why a config file could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not valid TOML: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not serialise config: {0}")]
    Encode(#[from] toml::ser::Error),
    #[error("invalid action: {0}")]
    Action(#[from] crate::action::ActionError),
    #[error("`blacklist_regex` is not a valid regular expression: {0}")]
    BadRegex(#[from] regex::Error),
    #[error("{field} must be greater than zero")]
    NotPositive { field: &'static str },
    #[error("duplicate conversion id `{id}`")]
    DuplicateConversion { id: String },
    #[error("conversion `{id}` has an empty `to`; it must name a target format")]
    ConversionWithoutTarget { id: String },
    #[error("duplicate preset id `{id}`")]
    DuplicatePreset { id: String },
    #[error("preset `{id}` has `{field} = \"{value}\"`, which is not a hex colour like `#3b5bdb`")]
    BadPresetColor {
        id: String,
        field: &'static str,
        value: String,
    },
    #[error("shortener `{name}` is `{kind}` but is missing `{field}`")]
    IncompleteShortener {
        name: String,
        kind: &'static str,
        field: &'static str,
    },
}

impl Config {
    /// Checks everything that can be checked without doing any work, and returns
    /// the validated action list.
    ///
    /// Called once at startup and again after a reload, so that a broken edit is
    /// reported as a notification instead of taking effect halfway.
    ///
    /// # Errors
    ///
    /// Returns the first problem found, phrased to name the offending field,
    /// action, preset or shortener.
    pub fn validate(&self) -> Result<Vec<Action>, ConfigError> {
        if self.split.default_limit == 0 {
            return Err(ConfigError::NotPositive {
                field: "split.default_limit",
            });
        }
        if self.truncate.default_limit == 0 {
            return Err(ConfigError::NotPositive {
                field: "truncate.default_limit",
            });
        }
        if !self.blacklist_regex.trim().is_empty() {
            regex::Regex::new(&self.blacklist_regex)?;
        }

        let mut seen = BTreeSet::new();
        for preset in &self.presets {
            if !seen.insert(&preset.id) {
                return Err(ConfigError::DuplicatePreset {
                    id: preset.id.clone(),
                });
            }
            for (field, value) in [
                ("button_color", preset.button_color.as_ref()),
                ("text_color", preset.text_color.as_ref()),
            ] {
                if !crate::color::is_valid(value) {
                    return Err(ConfigError::BadPresetColor {
                        id: preset.id.clone(),
                        field,
                        value: value.cloned().unwrap_or_default(),
                    });
                }
            }
        }

        let mut seen = BTreeSet::new();
        for conversion in &self.conversions {
            if !seen.insert(&conversion.id) {
                return Err(ConfigError::DuplicateConversion {
                    id: conversion.id.clone(),
                });
            }
            if conversion.to.trim().is_empty() {
                return Err(ConfigError::ConversionWithoutTarget {
                    id: conversion.id.clone(),
                });
            }
        }

        for shortener in self.shorteners.iter().filter(|s| s.enabled) {
            let missing = match shortener.kind {
                ShortenerKind::Yourls if shortener.api_url.trim().is_empty() => Some("api_url"),
                ShortenerKind::Yourls if shortener.signature.trim().is_empty() => Some("signature"),
                ShortenerKind::Command if shortener.command.is_empty() => Some("command"),
                _ => None,
            };
            if let Some(field) = missing {
                return Err(ConfigError::IncompleteShortener {
                    name: shortener.name.clone(),
                    kind: match shortener.kind {
                        ShortenerKind::Yourls => "yourls",
                        ShortenerKind::Command => "command",
                    },
                    field,
                });
            }
        }

        Ok(crate::action::validate_all(&self.actions)?)
    }

    /// Converts every icon written as a URL or a path into base64, in place.
    ///
    /// Returns whether anything changed, so the caller knows to save. Doing
    /// this once at load means the config keeps working if the source moves,
    /// and that opening a menu never touches the network or the disk.
    ///
    /// A broken icon is dropped with a warning rather than failing the load: a
    /// missing picture is not a reason to refuse to start.
    pub fn resolve_icons(&mut self) -> bool {
        let mut changed = false;

        for preset in &mut self.presets {
            changed |= resolve_icon(&mut preset.icon, "preset", &preset.id);
        }
        for action in &mut self.actions {
            changed |= resolve_icon(&mut action.icon, "action", &action.id);
        }
        changed
    }

    /// The shorteners `Shorten` and auto-shorten are allowed to pick from.
    #[must_use]
    pub fn active_shorteners(&self) -> Vec<&Shortener> {
        self.shorteners.iter().filter(|s| s.enabled).collect()
    }
}

/// Resolves one icon field, reporting what happened.
fn resolve_icon(icon: &mut Option<String>, what: &str, id: &str) -> bool {
    let Some(raw) = icon.clone() else {
        return false;
    };

    match crate::icons::resolve(&raw) {
        Ok(Some(encoded)) => {
            log::info!("{what} `{id}`: icon converted and cached in the config");
            *icon = Some(encoded);
            true
        }
        Ok(None) => false,
        Err(e) => {
            log::warn!("{what} `{id}`: ignoring its icon: {e}");
            *icon = None;
            true
        }
    }
}

/// Where the config file lives.
///
/// A `config.toml` beside the executable wins, which keeps a portable copy
/// self-contained; otherwise the XDG location is used.
#[must_use]
pub fn config_path() -> PathBuf {
    if let Ok(mut exe) = std::env::current_exe() {
        exe.pop();
        let beside = exe.join("config.toml");
        if beside.is_file() {
            return beside;
        }
    }

    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));

    base.join("clip-convert").join("config.toml")
}

/// Reads the config at `path`, writing a commented default file if none exists.
///
/// # Errors
///
/// Returns [`ConfigError::Read`] or [`ConfigError::Parse`] for an unusable
/// file, or [`ConfigError::Write`] if the default file could not be created.
pub fn load_from(path: &std::path::Path) -> Result<Config, ConfigError> {
    if !path.exists() {
        let config = Config::default();
        save_to(path, &config)?;
        return Ok(config);
    }

    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// Writes `config` to `path`, creating the parent directory if needed.
///
/// # Errors
///
/// Returns [`ConfigError::Write`] if the directory or file cannot be written,
/// or [`ConfigError::Encode`] if the config cannot be serialised.
pub fn save_to(path: &std::path::Path, config: &Config) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let text = toml::to_string_pretty(config)?;
    std::fs::write(path, text).map_err(|source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::ContentKind;

    #[test]
    fn the_default_config_is_valid() {
        Config::default()
            .validate()
            .expect("factory config must load");
    }

    #[test]
    fn the_default_config_survives_a_round_trip() {
        let original = Config::default();
        let text = toml::to_string_pretty(&original).expect("encodable");
        let decoded: Config = toml::from_str(&text).expect("decodable");
        assert_eq!(decoded, original);
    }

    #[test]
    fn an_empty_file_yields_the_factory_defaults() {
        let decoded: Config = toml::from_str("").expect("all fields have defaults");
        assert_eq!(decoded, Config::default());
    }

    #[test]
    fn a_misspelled_key_is_rejected() {
        let err = toml::from_str::<Config>("auto_shortne = true").expect_err("typo");
        assert!(err.to_string().contains("auto_shortne"), "{err}");
    }

    #[test]
    fn factory_actions_cover_every_content_kind() {
        let actions = Config::default().validate().expect("valid");
        for kind in ContentKind::all() {
            assert!(
                !crate::action::for_kinds(&actions, &BTreeSet::from([kind])).is_empty(),
                "no action offered for {kind}"
            );
        }
    }

    #[test]
    fn the_factory_url_menu_is_the_documented_set() {
        let actions = Config::default().validate().expect("valid");
        let ids: Vec<&str> =
            crate::action::for_kinds(&actions, &BTreeSet::from([ContentKind::Url]))
                .iter()
                .map(|a| a.id.as_str())
                .collect();
        // Convert is absent: nothing in the factory table turns a URL into
        // anything else.
        assert_eq!(ids, ["type", "shorten", "split", "truncate", "replace"]);
    }

    #[test]
    fn the_factory_image_menu_is_the_documented_set() {
        let actions = Config::default().validate().expect("valid");
        let ids: Vec<&str> =
            crate::action::for_kinds(&actions, &BTreeSet::from([ContentKind::Image]))
                .iter()
                .map(|a| a.id.as_str())
                .collect();
        assert_eq!(ids, ["type", "resize", "convert"]);
    }

    #[test]
    fn shorten_is_not_offered_for_plain_text() {
        let actions = Config::default().validate().expect("valid");
        let ids: Vec<&str> =
            crate::action::for_kinds(&actions, &BTreeSet::from([ContentKind::Text]))
                .iter()
                .map(|a| a.id.as_str())
                .collect();
        assert_eq!(ids, ["type", "split", "truncate", "replace"]);
    }

    #[test]
    fn a_zero_limit_is_rejected_rather_than_panicking_later() {
        let config = Config {
            split: SplitSettings {
                default_limit: 0,
                ..SplitSettings::default()
            },
            ..Config::default()
        };
        assert!(matches!(
            config.validate(),
            Err(ConfigError::NotPositive {
                field: "split.default_limit"
            })
        ));
    }

    #[test]
    fn an_invalid_blacklist_regex_is_reported() {
        let config = Config {
            blacklist_regex: "([".to_string(),
            ..Config::default()
        };
        assert!(matches!(config.validate(), Err(ConfigError::BadRegex(_))));
    }

    #[test]
    fn an_incomplete_shortener_is_reported_with_the_missing_field() {
        let mut config = Config::default();
        config.shorteners.push(Shortener {
            name: "mine".to_string(),
            kind: ShortenerKind::Yourls,
            api_url: String::new(),
            signature: "x".to_string(),
            base_url: String::new(),
            command: Vec::new(),
            enabled: true,
        });
        match config.validate() {
            Err(ConfigError::IncompleteShortener { field, name, .. }) => {
                assert_eq!(field, "api_url");
                assert_eq!(name, "mine");
            }
            other => panic!("expected IncompleteShortener, got {other:?}"),
        }
    }

    #[test]
    fn a_disabled_shortener_is_not_validated_or_offered() {
        let mut config = Config::default();
        config.shorteners.push(Shortener {
            name: "broken".to_string(),
            kind: ShortenerKind::Yourls,
            api_url: String::new(),
            signature: String::new(),
            base_url: String::new(),
            command: Vec::new(),
            enabled: false,
        });
        assert!(config.validate().is_ok());
        assert!(config.active_shorteners().is_empty());
    }

    #[test]
    fn a_colour_that_is_not_a_colour_is_reported_by_entry_and_field() {
        let mut config = Config::default();
        config.presets[0].button_color = Some("octarine".to_string());
        match config.validate() {
            Err(ConfigError::BadPresetColor { id, field, .. }) => {
                assert_eq!(id, config.presets[0].id);
                assert_eq!(field, "button_color");
            }
            other => panic!("expected BadPresetColor, got {other:?}"),
        }

        config.presets[0].button_color = Some("#3b5bdb".to_string());
        config.actions[0].text_color = Some("#not-a-colour".to_string());
        match config.validate() {
            Err(ConfigError::Action(crate::action::ActionError::BadColor { field, .. })) => {
                assert_eq!(field, "text_color");
            }
            other => panic!("expected a bad action colour, got {other:?}"),
        }
    }

    #[test]
    fn a_configured_colour_survives_a_round_trip() {
        let mut config = Config::default();
        config.actions[0].button_color = Some("#3b5bdb".to_string());
        config.presets[0].text_color = Some("#fff".to_string());
        let text = toml::to_string_pretty(&config).expect("encodable");
        let decoded: Config = toml::from_str(&text).expect("decodable");
        assert_eq!(decoded, config);
        config.validate().expect("hex colours are valid");
    }

    #[test]
    fn a_conversion_without_a_target_is_rejected_by_id() {
        let mut config = Config::default();
        config.conversions[0].to = String::new();
        match config.validate() {
            Err(ConfigError::ConversionWithoutTarget { id }) => {
                assert_eq!(id, config.conversions[0].id);
            }
            other => panic!("expected ConversionWithoutTarget, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_conversions_are_rejected() {
        let mut config = Config::default();
        let first = config.conversions[0].clone();
        config.conversions.push(first);
        assert!(matches!(
            config.validate(),
            Err(ConfigError::DuplicateConversion { .. })
        ));
    }

    #[test]
    fn duplicate_presets_are_rejected() {
        let mut config = Config::default();
        let first = config.presets[0].clone();
        config.presets.push(first);
        assert!(matches!(
            config.validate(),
            Err(ConfigError::DuplicatePreset { .. })
        ));
    }

    #[test]
    fn a_missing_file_is_created_with_the_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("config.toml");

        let loaded = load_from(&path).expect("creates the file");
        assert_eq!(loaded, Config::default());
        assert!(path.is_file(), "default config should have been written");

        let reloaded = load_from(&path).expect("reads it back");
        assert_eq!(reloaded, loaded);
    }

    #[test]
    fn edits_made_by_hand_are_preserved_across_a_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");

        let mut config = load_from(&path).expect("create");
        config.actions.push(ActionSpec {
            id: "ocr".to_string(),
            label: "OCR".to_string(),
            when: vec!["image".to_string()],
            builtin: None,
            command: vec!["tesseract".to_string(), "-".to_string(), "-".to_string()],
            input: InputMode::Stdin,
            output: OutputMode::Clipboard,
            enabled: true,
            icon: None,
            button_color: None,
            text_color: None,
        });
        config.auto_shorten = false;
        save_to(&path, &config).expect("save");

        let reloaded = load_from(&path).expect("reload");
        assert_eq!(reloaded, config);
        assert!(!reloaded.auto_shorten);

        let actions = reloaded.validate().expect("custom action is valid");
        let image = crate::action::for_kinds(&actions, &BTreeSet::from([ContentKind::Image]));
        assert!(image.iter().any(|a| a.id == "ocr"));
    }

    #[test]
    fn a_parse_error_names_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not toml {{{").expect("write");

        match load_from(&path) {
            Err(ConfigError::Parse { path: p, .. }) => assert_eq!(p, path),
            other => panic!("expected a parse error, got {other:?}"),
        }
    }
}

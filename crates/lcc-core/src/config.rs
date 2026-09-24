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
use std::collections::BTreeSet;
use std::path::PathBuf;

pub(crate) const fn default_true() -> bool {
    true
}

/// External programs the built-in actions drive.
///
/// These are argument vectors rather than shell strings, so clipboard content
/// never reaches a shell. `{...}` placeholders are substituted by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Commands {
    /// Types text read from standard input. `{delay}` is the per-key delay in ms.
    pub type_text: Vec<String>,
    /// Presses Return, used between the pieces of a split.
    pub key_enter: Vec<String>,
    /// Presses the paste chord, used by "Paste after action".
    pub key_paste: Vec<String>,
    /// Prints `WIDTH HEIGHT` for `{input}`.
    pub identify: Vec<String>,
    /// Scales `{input}` to `{geometry}` at `{quality}`, writing `{output}`.
    pub convert_inside: Vec<String>,
    /// As above, then pads to exactly `{width}x{height}` on a transparent canvas.
    pub convert_exact: Vec<String>,
}

impl Default for Commands {
    fn default() -> Self {
        let v = |parts: &[&str]| parts.iter().map(|s| (*s).to_string()).collect();
        Self {
            // ydotool works under Wayland, where a compositor will not let an
            // ordinary client synthesise input. It needs ydotoold running.
            type_text: v(&["ydotool", "type", "--key-delay", "{delay}", "--file", "-"]),
            key_enter: v(&["ydotool", "key", "28:1", "28:0"]),
            key_paste: v(&["ydotool", "key", "29:1", "47:1", "47:0", "29:0"]),
            identify: v(&["magick", "identify", "-format", "%w %h", "{input}"]),
            convert_inside: v(&[
                "magick", "{input}", "-resize", "{geometry}", "-quality", "{quality}", "{output}",
            ]),
            convert_exact: v(&[
                "magick",
                "{input}",
                "-resize",
                "{geometry}",
                "-background",
                "none",
                "-gravity",
                "center",
                "-extent",
                "{width}x{height}",
                "-quality",
                "{quality}",
                "{output}",
            ]),
        }
    }
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
    /// Per-key delay for typing, in ms. Higher is slower but more reliable in
    /// apps that drop synthesised keys.
    #[serde(default = "default_type_delay")]
    pub type_delay_ms: u32,
    #[serde(default)]
    pub split: SplitSettings,
    #[serde(default)]
    pub truncate: TruncateSettings,
    #[serde(default)]
    pub commands: Commands,
    #[serde(default)]
    pub shorteners: Vec<Shortener>,
    #[serde(default = "presets::factory")]
    pub presets: Vec<Preset>,
    #[serde(default = "factory_actions")]
    pub actions: Vec<ActionSpec>,
}

fn default_hotkey() -> String {
    "ctrl+b".to_string()
}

const fn default_type_delay() -> u32 {
    12
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
            type_delay_ms: default_type_delay(),
            split: SplitSettings::default(),
            truncate: TruncateSettings::default(),
            commands: Commands::default(),
            shorteners: Vec::new(),
            presets: presets::factory(),
            actions: factory_actions(),
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
    };

    vec![
        // Offered for every kind: with an image on the clipboard it reports that
        // there is nothing to type rather than being silently absent.
        builtin("type", "Type", &["any"], Builtin::Type),
        builtin("shorten", "Shorten", &["url"], Builtin::Shorten),
        builtin("split", "Split", &["text", "url"], Builtin::Split),
        builtin("truncate", "Truncate", &["text", "url"], Builtin::Truncate),
        builtin("resize", "Resize", &["image"], Builtin::Resize),
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
    #[error("duplicate preset id `{id}`")]
    DuplicatePreset { id: String },
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

    /// The shorteners `Shorten` and auto-shorten are allowed to pick from.
    #[must_use]
    pub fn active_shorteners(&self) -> Vec<&Shortener> {
        self.shorteners.iter().filter(|s| s.enabled).collect()
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

    base.join("linux-clip-convert").join("config.toml")
}

/// Reads the config at `path`, writing a commented default file if none exists.
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
        Config::default().validate().expect("factory config must load");
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
                !crate::action::for_kind(&actions, kind).is_empty(),
                "no action offered for {kind}"
            );
        }
    }

    #[test]
    fn the_factory_url_menu_is_the_documented_set() {
        let actions = Config::default().validate().expect("valid");
        let ids: Vec<&str> = crate::action::for_kind(&actions, ContentKind::Url)
            .iter()
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(ids, ["type", "shorten", "split", "truncate"]);
    }

    #[test]
    fn the_factory_image_menu_is_the_documented_set() {
        let actions = Config::default().validate().expect("valid");
        let ids: Vec<&str> = crate::action::for_kind(&actions, ContentKind::Image)
            .iter()
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(ids, ["type", "resize"]);
    }

    #[test]
    fn shorten_is_not_offered_for_plain_text() {
        let actions = Config::default().validate().expect("valid");
        let ids: Vec<&str> = crate::action::for_kind(&actions, ContentKind::Text)
            .iter()
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(ids, ["type", "split", "truncate"]);
    }

    #[test]
    fn a_zero_limit_is_rejected_rather_than_panicking_later() {
        let mut config = Config::default();
        config.split.default_limit = 0;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::NotPositive {
                field: "split.default_limit"
            })
        ));
    }

    #[test]
    fn an_invalid_blacklist_regex_is_reported() {
        let mut config = Config::default();
        config.blacklist_regex = "([".to_string();
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
        });
        config.auto_shorten = false;
        save_to(&path, &config).expect("save");

        let reloaded = load_from(&path).expect("reload");
        assert_eq!(reloaded, config);
        assert!(!reloaded.auto_shorten);

        let actions = reloaded.validate().expect("custom action is valid");
        let image = crate::action::for_kind(&actions, ContentKind::Image);
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

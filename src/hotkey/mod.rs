//! Global hotkey capture.
//!
//! The chord is parsed into a platform-neutral [`Chord`] here, and each backend
//! maps that onto its own key codes. Parsing is the part with all the edge
//! cases, so keeping it platform-free means it is tested once and behaves the
//! same everywhere.
//!
//! The backends differ because the platforms genuinely do. Windows, macOS and
//! X11 offer a real global-shortcut API. Wayland deliberately does not, so on
//! Linux the keys are read from evdev instead, which needs membership of the
//! `input` group — reported at startup rather than failing silently.

#[cfg(not(target_os = "linux"))]
mod desktop;
#[cfg(target_os = "linux")]
mod linux;

use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::sync::mpsc::Sender;
use std::sync::Arc;

#[cfg(not(target_os = "linux"))]
pub use desktop::Modifiers;
#[cfg(target_os = "linux")]
pub use linux::Modifiers;

/// A modifier key, independent of any platform's numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Meta,
}

impl Modifier {
    pub(crate) const fn all() -> [Self; 4] {
        [Self::Ctrl, Self::Shift, Self::Alt, Self::Meta]
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "ctrl" | "control" => Some(Self::Ctrl),
            "shift" => Some(Self::Shift),
            "alt" | "option" => Some(Self::Alt),
            "meta" | "super" | "win" | "cmd" | "command" => Some(Self::Meta),
            _ => None,
        }
    }
}

/// A non-modifier key, named rather than numbered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyName {
    /// An ASCII letter, always stored lowercase.
    Letter(char),
    Digit(char),
    /// F1 to F12.
    Function(u8),
    Space,
    Enter,
    Tab,
    Escape,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Backspace,
}

impl KeyName {
    fn parse(name: &str) -> Option<Self> {
        if let Some(c) = name.chars().next().filter(|_| name.chars().count() == 1) {
            if c.is_ascii_lowercase() {
                return Some(Self::Letter(c));
            }
            if c.is_ascii_digit() {
                return Some(Self::Digit(c));
            }
        }

        if let Some(number) = name.strip_prefix('f') {
            if let Ok(n) = number.parse::<u8>() {
                if (1..=12).contains(&n) {
                    return Some(Self::Function(n));
                }
            }
        }

        Some(match name {
            "space" => Self::Space,
            "enter" | "return" => Self::Enter,
            "tab" => Self::Tab,
            "escape" | "esc" => Self::Escape,
            "insert" => Self::Insert,
            "delete" | "del" => Self::Delete,
            "home" => Self::Home,
            "end" => Self::End,
            "pageup" => Self::PageUp,
            "pagedown" => Self::PageDown,
            "backspace" => Self::Backspace,
            _ => return None,
        })
    }
}

/// A parsed chord, e.g. `ctrl+b`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub modifiers: BTreeSet<Modifier>,
    pub key: KeyName,
}

/// Parses a chord written as `ctrl+b`, `ctrl+shift+f1`, and so on.
///
/// # Errors
///
/// Returns an error naming the unrecognised part, so a typo in the config is
/// reported rather than leaving the hotkey mysteriously dead.
pub fn parse_chord(spec: &str) -> Result<Chord> {
    let mut modifiers = BTreeSet::new();
    let mut key = None;

    for part in spec.split('+') {
        let part = part.trim().to_ascii_lowercase();
        if part.is_empty() {
            continue;
        }
        if let Some(modifier) = Modifier::parse(&part) {
            modifiers.insert(modifier);
        } else if let Some(found) = KeyName::parse(&part) {
            if key.is_some() {
                bail!("hotkey `{spec}` names more than one non-modifier key");
            }
            key = Some(found);
        } else {
            bail!("hotkey `{spec}` contains an unrecognised key `{part}`");
        }
    }

    let key = key.with_context(|| format!("hotkey `{spec}` has no non-modifier key"))?;
    Ok(Chord { modifiers, key })
}

/// Starts listening for `chord`, reporting presses on `events`.
///
/// # Errors
///
/// Returns an error if the platform's hotkey mechanism is unavailable, with a
/// message explaining what the user can do about it.
pub fn listen(chord: &Chord, modifiers: &Arc<Modifiers>, events: &Sender<()>) -> Result<String> {
    #[cfg(target_os = "linux")]
    {
        linux::listen(chord, modifiers, events)
    }
    #[cfg(not(target_os = "linux"))]
    {
        desktop::listen(chord, modifiers, events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_chord_parses() {
        let chord = parse_chord("ctrl+b").expect("valid");
        assert_eq!(chord.key, KeyName::Letter('b'));
        assert_eq!(chord.modifiers, BTreeSet::from([Modifier::Ctrl]));
    }

    #[test]
    fn parsing_ignores_case_and_spacing() {
        assert_eq!(
            parse_chord("  CTRL + B ").expect("valid"),
            parse_chord("ctrl+b").expect("valid")
        );
    }

    #[test]
    fn several_modifiers_are_accepted() {
        let chord = parse_chord("ctrl+shift+alt+k").expect("valid");
        assert_eq!(chord.key, KeyName::Letter('k'));
        assert_eq!(chord.modifiers.len(), 3);
    }

    #[test]
    fn modifier_aliases_agree_across_platforms() {
        // A macOS user writes cmd; a Windows user writes win. Same chord.
        for alias in ["meta+b", "super+b", "win+b", "cmd+b", "command+b"] {
            assert_eq!(
                parse_chord(alias).expect("valid").modifiers,
                BTreeSet::from([Modifier::Meta]),
                "{alias}"
            );
        }
        assert_eq!(
            parse_chord("option+b").expect("valid").modifiers,
            BTreeSet::from([Modifier::Alt])
        );
    }

    #[test]
    fn every_letter_is_distinct() {
        let mut seen = BTreeSet::new();
        for c in 'a'..='z' {
            let chord = parse_chord(&format!("ctrl+{c}")).expect("valid");
            assert!(seen.insert(chord.key), "{c} collided");
        }
        assert_eq!(seen.len(), 26);
    }

    #[test]
    fn function_keys_are_bounded() {
        assert_eq!(parse_chord("f1").expect("valid").key, KeyName::Function(1));
        assert_eq!(
            parse_chord("f12").expect("valid").key,
            KeyName::Function(12)
        );
        assert!(parse_chord("f13").is_err(), "f13 is not a known key");
        assert!(parse_chord("f0").is_err(), "f0 is not a known key");
    }

    #[test]
    fn named_keys_parse() {
        assert_eq!(
            parse_chord("ctrl+space").expect("valid").key,
            KeyName::Space
        );
        assert_eq!(parse_chord("ctrl+esc").expect("valid").key, KeyName::Escape);
        assert_eq!(parse_chord("alt+enter").expect("valid").key, KeyName::Enter);
        assert_eq!(
            parse_chord("ctrl+pagedown").expect("valid").key,
            KeyName::PageDown
        );
    }

    #[test]
    fn digits_parse() {
        assert_eq!(
            parse_chord("ctrl+0").expect("valid").key,
            KeyName::Digit('0')
        );
        assert_eq!(
            parse_chord("ctrl+9").expect("valid").key,
            KeyName::Digit('9')
        );
    }

    #[test]
    fn a_typo_is_reported_by_name() {
        let err = parse_chord("ctrl+bb").expect_err("not a key");
        assert!(err.to_string().contains("bb"), "{err}");
    }

    #[test]
    fn a_chord_with_no_real_key_is_rejected() {
        let err = parse_chord("ctrl+shift").expect_err("no key");
        assert!(err.to_string().contains("no non-modifier key"), "{err}");
    }

    #[test]
    fn two_non_modifier_keys_are_rejected() {
        assert!(parse_chord("ctrl+a+b").is_err());
    }
}

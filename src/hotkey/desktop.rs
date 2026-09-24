//! Hotkey capture on Windows and macOS, via the system's own shortcut API.
//!
//! > **Status:** written but not exercised — this project has so far only been
//! > run on Linux. The Linux path is the verified one.
//!
//! Unlike the evdev backend, the operating system reports only the registered
//! chord, never individual key transitions. Live modifier state is therefore not
//! available, so the "hold Shift to skip auto-shortening" rule cannot work here;
//! [`Modifiers::shift_held`] always reports false and the rule is simply never
//! triggered rather than behaving unpredictably.

use super::{Chord, KeyName, Modifier};
use anyhow::{Context, Result};
use global_hotkey::hotkey::{Code, HotKey, Modifiers as SystemModifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// Stands in for the evdev modifier tracker.
///
/// The system shortcut API does not expose key state, so this reports nothing
/// held. See the module note.
#[derive(Debug, Default)]
pub struct Modifiers;

impl Modifiers {
    /// Always false here: see the module documentation.
    #[must_use]
    pub const fn shift_held(&self) -> bool {
        false
    }
}

/// Scroll Lock state is not read on these platforms, so the rule never fires.
#[must_use]
pub const fn scroll_lock_on() -> bool {
    false
}

/// Maps a platform-neutral key name onto a `KeyboardEvent` code.
fn to_code(name: KeyName) -> Option<Code> {
    const LETTERS: [(char, Code); 26] = [
        ('a', Code::KeyA),
        ('b', Code::KeyB),
        ('c', Code::KeyC),
        ('d', Code::KeyD),
        ('e', Code::KeyE),
        ('f', Code::KeyF),
        ('g', Code::KeyG),
        ('h', Code::KeyH),
        ('i', Code::KeyI),
        ('j', Code::KeyJ),
        ('k', Code::KeyK),
        ('l', Code::KeyL),
        ('m', Code::KeyM),
        ('n', Code::KeyN),
        ('o', Code::KeyO),
        ('p', Code::KeyP),
        ('q', Code::KeyQ),
        ('r', Code::KeyR),
        ('s', Code::KeyS),
        ('t', Code::KeyT),
        ('u', Code::KeyU),
        ('v', Code::KeyV),
        ('w', Code::KeyW),
        ('x', Code::KeyX),
        ('y', Code::KeyY),
        ('z', Code::KeyZ),
    ];
    const DIGITS: [(char, Code); 10] = [
        ('0', Code::Digit0),
        ('1', Code::Digit1),
        ('2', Code::Digit2),
        ('3', Code::Digit3),
        ('4', Code::Digit4),
        ('5', Code::Digit5),
        ('6', Code::Digit6),
        ('7', Code::Digit7),
        ('8', Code::Digit8),
        ('9', Code::Digit9),
    ];
    const FUNCTION: [Code; 12] = [
        Code::F1,
        Code::F2,
        Code::F3,
        Code::F4,
        Code::F5,
        Code::F6,
        Code::F7,
        Code::F8,
        Code::F9,
        Code::F10,
        Code::F11,
        Code::F12,
    ];

    Some(match name {
        KeyName::Letter(c) => LETTERS.iter().find(|(l, _)| *l == c)?.1,
        KeyName::Digit(c) => DIGITS.iter().find(|(d, _)| *d == c)?.1,
        KeyName::Function(n) => *FUNCTION.get(usize::from(n).checked_sub(1)?)?,
        KeyName::Space => Code::Space,
        KeyName::Enter => Code::Enter,
        KeyName::Tab => Code::Tab,
        KeyName::Escape => Code::Escape,
        KeyName::Insert => Code::Insert,
        KeyName::Delete => Code::Delete,
        KeyName::Home => Code::Home,
        KeyName::End => Code::End,
        KeyName::PageUp => Code::PageUp,
        KeyName::PageDown => Code::PageDown,
        KeyName::Backspace => Code::Backspace,
    })
}

/// Translates the parsed modifiers into the system's bitflags.
fn to_system_modifiers(chord: &Chord) -> SystemModifiers {
    let mut out = SystemModifiers::empty();
    for modifier in &chord.modifiers {
        out |= match modifier {
            Modifier::Ctrl => SystemModifiers::CONTROL,
            Modifier::Shift => SystemModifiers::SHIFT,
            Modifier::Alt => SystemModifiers::ALT,
            Modifier::Meta => SystemModifiers::META,
        };
    }
    out
}

/// Registers the chord with the system and reports presses on `events`.
///
/// # Errors
///
/// Returns an error if the chord cannot be represented, or if the system
/// refused to register it — most often because another application already
/// owns that combination.
pub fn listen(chord: &Chord, _modifiers: &Arc<Modifiers>, events: &Sender<()>) -> Result<String> {
    let code = to_code(chord.key)
        .ok_or_else(|| anyhow::anyhow!("that key cannot be registered as a global shortcut"))?;
    let hotkey = HotKey::new(Some(to_system_modifiers(chord)), code);

    let manager = GlobalHotKeyManager::new().context("creating the global hotkey manager")?;
    manager
        .register(hotkey)
        .context("registering the hotkey; another application may already use it")?;

    let events = events.clone();
    std::thread::spawn(move || {
        // The manager must outlive the registration, so it is moved in here.
        let _manager = manager;
        let receiver = GlobalHotKeyEvent::receiver();
        while let Ok(event) = receiver.recv() {
            if event.state == global_hotkey::HotKeyState::Pressed && events.send(()).is_err() {
                break;
            }
        }
    });

    Ok("system global shortcut".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_key_maps_to_a_distinct_code() {
        let mut seen = BTreeSet::new();
        for c in 'a'..='z' {
            assert!(seen.insert(format!(
                "{:?}",
                to_code(KeyName::Letter(c)).expect("mapped")
            )));
        }
        for c in '0'..='9' {
            assert!(seen.insert(format!("{:?}", to_code(KeyName::Digit(c)).expect("mapped"))));
        }
        for n in 1..=12u8 {
            assert!(seen.insert(format!(
                "{:?}",
                to_code(KeyName::Function(n)).expect("mapped")
            )));
        }
    }

    #[test]
    fn an_out_of_range_function_key_does_not_panic() {
        assert_eq!(to_code(KeyName::Function(0)), None);
        assert_eq!(to_code(KeyName::Function(99)), None);
    }

    #[test]
    fn modifiers_translate_to_the_system_flags() {
        let chord = super::super::parse_chord("ctrl+shift+b").expect("valid");
        let flags = to_system_modifiers(&chord);
        assert!(flags.contains(SystemModifiers::CONTROL));
        assert!(flags.contains(SystemModifiers::SHIFT));
        assert!(!flags.contains(SystemModifiers::ALT));
    }

    #[test]
    fn the_shift_bypass_is_inert_rather_than_wrong() {
        // Documented limitation: without key-state access the rule cannot work,
        // so it must report "not held" rather than guess.
        assert!(!Modifiers::default().shift_held());
        assert!(!scroll_lock_on());
    }
}

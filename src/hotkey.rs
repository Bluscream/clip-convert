//! Global hotkey capture, and the live modifier state the bypass rules need.
//!
//! There is no portable way for a Wayland client to register a global shortcut,
//! so keys are read from the evdev devices directly. That requires membership of
//! the `input` group, which is checked and reported at startup rather than
//! failing silently.
//!
//! Each keyboard gets a thread blocked in `fetch_events`, so the listener costs
//! nothing while nobody is typing — unlike polling the keyboard state on a
//! timer, which wakes the CPU tens of times a second forever.

use anyhow::{bail, Context, Result};
use evdev::{Device, InputEventKind, Key};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// Modifier keys, tracked globally because the bypass rules need to know what is
/// held at the moment an unrelated event (a clipboard change) arrives.
#[derive(Debug, Default)]
pub struct Modifiers {
    ctrl: AtomicBool,
    shift: AtomicBool,
    alt: AtomicBool,
    meta: AtomicBool,
}

impl Modifiers {
    fn set(&self, key: Key, pressed: bool) -> bool {
        let flag = match key {
            Key::KEY_LEFTCTRL | Key::KEY_RIGHTCTRL => &self.ctrl,
            Key::KEY_LEFTSHIFT | Key::KEY_RIGHTSHIFT => &self.shift,
            Key::KEY_LEFTALT | Key::KEY_RIGHTALT => &self.alt,
            Key::KEY_LEFTMETA | Key::KEY_RIGHTMETA => &self.meta,
            _ => return false,
        };
        flag.store(pressed, Ordering::Relaxed);
        true
    }

    /// Whether Shift is held right now.
    #[must_use]
    pub fn shift_held(&self) -> bool {
        self.shift.load(Ordering::Relaxed)
    }

    fn matches(&self, required: &BTreeSet<Modifier>) -> bool {
        let held = |m: Modifier| match m {
            Modifier::Ctrl => self.ctrl.load(Ordering::Relaxed),
            Modifier::Shift => self.shift.load(Ordering::Relaxed),
            Modifier::Alt => self.alt.load(Ordering::Relaxed),
            Modifier::Meta => self.meta.load(Ordering::Relaxed),
        };
        // Exact match: ctrl+b must not fire on ctrl+shift+b, which is very
        // likely to be a different shortcut in the focused application.
        Modifier::all()
            .into_iter()
            .all(|m| held(m) == required.contains(&m))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Meta,
}

impl Modifier {
    const fn all() -> [Self; 4] {
        [Self::Ctrl, Self::Shift, Self::Alt, Self::Meta]
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "ctrl" | "control" => Some(Self::Ctrl),
            "shift" => Some(Self::Shift),
            "alt" => Some(Self::Alt),
            "meta" | "super" | "win" => Some(Self::Meta),
            _ => None,
        }
    }
}

/// A parsed chord, e.g. `ctrl+b`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub modifiers: BTreeSet<Modifier>,
    pub key: Key,
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
        } else if let Some(found) = key_from_name(&part) {
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

/// Maps a key name to its evdev code.
///
/// The tables are explicit because evdev codes follow the physical QWERTY
/// layout rather than any alphabetical or numeric order: `KEY_A` is 30 but
/// `KEY_B` is 48, and `KEY_F11` jumps from 68 to 87. Computing them by offset
/// silently produces the wrong key.
fn key_from_name(name: &str) -> Option<Key> {
    const LETTERS: [(char, Key); 26] = [
        ('a', Key::KEY_A),
        ('b', Key::KEY_B),
        ('c', Key::KEY_C),
        ('d', Key::KEY_D),
        ('e', Key::KEY_E),
        ('f', Key::KEY_F),
        ('g', Key::KEY_G),
        ('h', Key::KEY_H),
        ('i', Key::KEY_I),
        ('j', Key::KEY_J),
        ('k', Key::KEY_K),
        ('l', Key::KEY_L),
        ('m', Key::KEY_M),
        ('n', Key::KEY_N),
        ('o', Key::KEY_O),
        ('p', Key::KEY_P),
        ('q', Key::KEY_Q),
        ('r', Key::KEY_R),
        ('s', Key::KEY_S),
        ('t', Key::KEY_T),
        ('u', Key::KEY_U),
        ('v', Key::KEY_V),
        ('w', Key::KEY_W),
        ('x', Key::KEY_X),
        ('y', Key::KEY_Y),
        ('z', Key::KEY_Z),
    ];
    const DIGITS: [(char, Key); 10] = [
        ('0', Key::KEY_0),
        ('1', Key::KEY_1),
        ('2', Key::KEY_2),
        ('3', Key::KEY_3),
        ('4', Key::KEY_4),
        ('5', Key::KEY_5),
        ('6', Key::KEY_6),
        ('7', Key::KEY_7),
        ('8', Key::KEY_8),
        ('9', Key::KEY_9),
    ];
    const FUNCTION: [(&str, Key); 12] = [
        ("f1", Key::KEY_F1),
        ("f2", Key::KEY_F2),
        ("f3", Key::KEY_F3),
        ("f4", Key::KEY_F4),
        ("f5", Key::KEY_F5),
        ("f6", Key::KEY_F6),
        ("f7", Key::KEY_F7),
        ("f8", Key::KEY_F8),
        ("f9", Key::KEY_F9),
        ("f10", Key::KEY_F10),
        ("f11", Key::KEY_F11),
        ("f12", Key::KEY_F12),
    ];

    if let Some(c) = name.chars().next().filter(|_| name.len() == 1) {
        if let Some((_, key)) = LETTERS.iter().find(|(letter, _)| *letter == c) {
            return Some(*key);
        }
        if let Some((_, key)) = DIGITS.iter().find(|(digit, _)| *digit == c) {
            return Some(*key);
        }
    }

    if let Some((_, key)) = FUNCTION.iter().find(|(label, _)| *label == name) {
        return Some(*key);
    }

    Some(match name {
        "space" => Key::KEY_SPACE,
        "enter" | "return" => Key::KEY_ENTER,
        "tab" => Key::KEY_TAB,
        "escape" | "esc" => Key::KEY_ESC,
        "insert" => Key::KEY_INSERT,
        "delete" | "del" => Key::KEY_DELETE,
        "home" => Key::KEY_HOME,
        "end" => Key::KEY_END,
        "pageup" => Key::KEY_PAGEUP,
        "pagedown" => Key::KEY_PAGEDOWN,
        "backspace" => Key::KEY_BACKSPACE,
        _ => return None,
    })
}

/// Starts listening for `chord` on every keyboard, reporting presses on `events`.
///
/// # Errors
///
/// Returns an error if no readable keyboard could be opened, which almost always
/// means the user is not in the `input` group.
pub fn listen(chord: &Chord, modifiers: &Arc<Modifiers>, events: &Sender<()>) -> Result<usize> {
    let keyboards: Vec<(std::path::PathBuf, Device)> = evdev::enumerate()
        .filter(|(_, device)| is_keyboard(device))
        .collect();

    if keyboards.is_empty() {
        bail!(
            "no readable keyboard found in /dev/input. Add your user to the `input` group \
             (`sudo usermod -aG input $USER`) and log back in."
        );
    }

    let count = keyboards.len();
    for (path, device) in keyboards {
        let chord = chord.clone();
        let modifiers = Arc::clone(modifiers);
        let events = events.clone();
        std::thread::spawn(move || {
            log::debug!("watching {} for {:?}", path.display(), chord.key);
            if let Err(e) = pump(device, &chord, &modifiers, &events) {
                // A device disappearing is normal — an unplugged keyboard, or a
                // Bluetooth one going to sleep.
                log::debug!("stopped watching {}: {e}", path.display());
            }
        });
    }

    Ok(count)
}

/// Reads one device forever, updating modifier state and firing on the chord.
fn pump(
    mut device: Device,
    chord: &Chord,
    modifiers: &Modifiers,
    events: &Sender<()>,
) -> Result<()> {
    loop {
        // Blocks here until the kernel has something; no timer, no wakeups.
        for event in device.fetch_events()? {
            let InputEventKind::Key(key) = event.kind() else {
                continue;
            };

            // 0 = released, 1 = pressed, 2 = auto-repeat.
            let pressed = event.value() != 0;
            if modifiers.set(key, pressed) {
                continue;
            }

            // Only a fresh press fires, so holding the chord does not repeat.
            if key == chord.key && event.value() == 1 && modifiers.matches(&chord.modifiers) {
                log::debug!("hotkey pressed");
                if events.send(()).is_err() {
                    return Ok(()); // The app is shutting down.
                }
            }
        }
    }
}

/// Whether a device looks like a keyboard rather than a mouse or gamepad.
fn is_keyboard(device: &Device) -> bool {
    device.supported_keys().is_some_and(|keys| {
        keys.contains(Key::KEY_A) && keys.contains(Key::KEY_Z) && keys.contains(Key::KEY_SPACE)
    })
}

/// Whether Scroll Lock is currently on, read from the keyboard LED state.
#[must_use]
pub fn scroll_lock_on() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/leds") else {
        return false;
    };

    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.contains("scrolllock"))
            && std::fs::read_to_string(entry.path().join("brightness"))
                .is_ok_and(|value| value.trim() != "0")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_chord_parses() {
        let chord = parse_chord("ctrl+b").expect("valid");
        assert_eq!(chord.key, Key::KEY_B);
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
        assert_eq!(chord.key, Key::KEY_K);
        assert_eq!(chord.modifiers.len(), 3);
    }

    #[test]
    fn modifier_aliases_agree() {
        for alias in ["meta+b", "super+b", "win+b"] {
            assert_eq!(
                parse_chord(alias).expect("valid").modifiers,
                BTreeSet::from([Modifier::Meta])
            );
        }
        assert_eq!(
            parse_chord("control+b").expect("valid"),
            parse_chord("ctrl+b").expect("valid")
        );
    }

    #[test]
    fn every_letter_maps_to_its_own_key() {
        let mut seen = BTreeSet::new();
        for c in 'a'..='z' {
            let chord = parse_chord(&format!("ctrl+{c}")).expect("valid");
            assert!(
                seen.insert(chord.key.code()),
                "{c} collided with another key"
            );
        }
        assert_eq!(seen.len(), 26);
        assert_eq!(parse_chord("ctrl+a").expect("valid").key, Key::KEY_A);
        assert_eq!(parse_chord("ctrl+z").expect("valid").key, Key::KEY_Z);
    }

    #[test]
    fn letter_and_function_keycodes_are_the_real_evdev_values() {
        // Regression: these were computed as an offset from KEY_A / KEY_F1,
        // which is wrong because evdev follows the physical QWERTY layout.
        assert_eq!(Key::KEY_A.code(), 30);
        assert_eq!(Key::KEY_B.code(), 48);
        assert_eq!(parse_chord("ctrl+b").expect("valid").key.code(), 48);
        assert_eq!(Key::KEY_F10.code(), 68);
        assert_eq!(Key::KEY_F11.code(), 87);
        assert_eq!(parse_chord("f11").expect("valid").key.code(), 87);
    }

    #[test]
    fn function_keys_map_correctly() {
        assert_eq!(parse_chord("f1").expect("valid").key, Key::KEY_F1);
        assert_eq!(parse_chord("f12").expect("valid").key, Key::KEY_F12);
        assert!(parse_chord("f13").is_err(), "f13 is not a known key");
    }

    #[test]
    fn named_keys_map_correctly() {
        assert_eq!(
            parse_chord("ctrl+space").expect("valid").key,
            Key::KEY_SPACE
        );
        assert_eq!(parse_chord("ctrl+esc").expect("valid").key, Key::KEY_ESC);
        assert_eq!(parse_chord("alt+enter").expect("valid").key, Key::KEY_ENTER);
    }

    #[test]
    fn digits_map_to_the_number_row() {
        assert_eq!(parse_chord("ctrl+0").expect("valid").key, Key::KEY_0);
        assert_eq!(parse_chord("ctrl+9").expect("valid").key, Key::KEY_9);
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

    #[test]
    fn modifier_matching_is_exact() {
        let modifiers = Modifiers::default();
        let required = BTreeSet::from([Modifier::Ctrl]);

        modifiers.set(Key::KEY_LEFTCTRL, true);
        assert!(
            modifiers.matches(&required),
            "ctrl alone should match ctrl+b"
        );

        // A superset must not fire: ctrl+shift+b usually means something else.
        modifiers.set(Key::KEY_LEFTSHIFT, true);
        assert!(!modifiers.matches(&required));

        modifiers.set(Key::KEY_LEFTSHIFT, false);
        assert!(modifiers.matches(&required));

        modifiers.set(Key::KEY_LEFTCTRL, false);
        assert!(
            !modifiers.matches(&required),
            "no modifiers should not match"
        );
    }

    #[test]
    fn left_and_right_modifiers_are_equivalent() {
        let modifiers = Modifiers::default();
        let required = BTreeSet::from([Modifier::Ctrl]);
        modifiers.set(Key::KEY_RIGHTCTRL, true);
        assert!(modifiers.matches(&required));
    }

    #[test]
    fn shift_state_is_visible_for_the_bypass_rule() {
        let modifiers = Modifiers::default();
        assert!(!modifiers.shift_held());
        modifiers.set(Key::KEY_RIGHTSHIFT, true);
        assert!(modifiers.shift_held());
        modifiers.set(Key::KEY_RIGHTSHIFT, false);
        assert!(!modifiers.shift_held());
    }

    #[test]
    fn a_non_modifier_key_is_not_treated_as_one() {
        let modifiers = Modifiers::default();
        assert!(!modifiers.set(Key::KEY_B, true));
    }

    #[test]
    fn reading_scroll_lock_never_panics() {
        let _ = scroll_lock_on();
    }
}

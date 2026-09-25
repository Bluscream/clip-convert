//! Reading the hotkey from evdev.
//!
//! Wayland offers no global-shortcut API to an ordinary client, so keys are read
//! from the input devices directly. Each keyboard gets a thread blocked in
//! `fetch_events`, so the listener costs nothing while nobody is typing —
//! unlike polling the keyboard state on a timer, which wakes the CPU tens of
//! times a second forever.
//!
//! This also provides the live modifier state the bypass rules need: the app has
//! to know whether Shift was held at the moment an unrelated event — a clipboard
//! change — arrived.

use super::{Chord, KeyName, Modifier};
use anyhow::{bail, Result};
use evdev::{Device, InputEventKind, Key};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// Live modifier state, built up as keys are seen.
#[derive(Debug, Default)]
pub struct Modifiers {
    ctrl: AtomicBool,
    shift: AtomicBool,
    alt: AtomicBool,
    meta: AtomicBool,
}

impl Modifiers {
    /// Records a key transition. Returns whether the key was a modifier.
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

    fn matches(&self, required: &BTreeSet<Modifier>) -> bool {
        let held = |m: Modifier| match m {
            Modifier::Ctrl => self.ctrl.load(Ordering::Relaxed),
            Modifier::Shift => self.shift.load(Ordering::Relaxed),
            Modifier::Alt => self.alt.load(Ordering::Relaxed),
            Modifier::Meta => self.meta.load(Ordering::Relaxed),
        };
        // Exact match: ctrl+b must not fire on ctrl+shift+b, which is very
        // likely a different shortcut in the focused application.
        Modifier::all()
            .into_iter()
            .all(|m| held(m) == required.contains(&m))
    }
}

/// Maps a platform-neutral key name onto its evdev code.
///
/// The tables are explicit because evdev codes follow the physical QWERTY
/// layout rather than any alphabetical or numeric order: `KEY_A` is 30 but
/// `KEY_B` is 48, and `KEY_F11` jumps from 68 to 87. Computing them by offset
/// silently produces the wrong key.
fn to_evdev(name: KeyName) -> Option<Key> {
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
    const FUNCTION: [Key; 12] = [
        Key::KEY_F1,
        Key::KEY_F2,
        Key::KEY_F3,
        Key::KEY_F4,
        Key::KEY_F5,
        Key::KEY_F6,
        Key::KEY_F7,
        Key::KEY_F8,
        Key::KEY_F9,
        Key::KEY_F10,
        Key::KEY_F11,
        Key::KEY_F12,
    ];

    Some(match name {
        KeyName::Letter(c) => LETTERS.iter().find(|(l, _)| *l == c)?.1,
        KeyName::Digit(c) => DIGITS.iter().find(|(d, _)| *d == c)?.1,
        KeyName::Function(n) => *FUNCTION.get(usize::from(n).checked_sub(1)?)?,
        KeyName::Space => Key::KEY_SPACE,
        KeyName::Enter => Key::KEY_ENTER,
        KeyName::Tab => Key::KEY_TAB,
        KeyName::Escape => Key::KEY_ESC,
        KeyName::Insert => Key::KEY_INSERT,
        KeyName::Delete => Key::KEY_DELETE,
        KeyName::Home => Key::KEY_HOME,
        KeyName::End => Key::KEY_END,
        KeyName::PageUp => Key::KEY_PAGEUP,
        KeyName::PageDown => Key::KEY_PAGEDOWN,
        KeyName::Backspace => Key::KEY_BACKSPACE,
    })
}

/// Starts listening on every keyboard.
///
/// # Errors
///
/// Returns an error if no readable keyboard could be opened, which almost always
/// means the user is not in the `input` group.
pub fn listen(chord: &Chord, modifiers: &Arc<Modifiers>, events: &Sender<()>) -> Result<String> {
    let target = to_evdev(chord.key)
        .ok_or_else(|| anyhow::anyhow!("that key cannot be watched on this platform"))?;

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
        let required = chord.modifiers.clone();
        let modifiers = Arc::clone(modifiers);
        let events = events.clone();
        std::thread::spawn(move || {
            if let Err(e) = pump(device, target, &required, &modifiers, &events) {
                // A device disappearing is normal: an unplugged keyboard, or a
                // Bluetooth one going to sleep.
                log::debug!("stopped watching {}: {e}", path.display());
            }
        });
    }

    Ok(format!("evdev on {count} keyboard(s)"))
}

/// Reads one device forever, updating modifier state and firing on the chord.
fn pump(
    mut device: Device,
    target: Key,
    required: &BTreeSet<Modifier>,
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
            if modifiers.set(key, event.value() != 0) {
                continue;
            }

            // Only a fresh press fires, so holding the chord does not repeat.
            if key == target && event.value() == 1 && modifiers.matches(required) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_and_function_keycodes_are_the_real_evdev_values() {
        // Regression: these were once computed as an offset from KEY_A / KEY_F1,
        // which is wrong because evdev follows the physical QWERTY layout. That
        // bug made ctrl+b listen for KEY_S.
        assert_eq!(Key::KEY_A.code(), 30);
        assert_eq!(Key::KEY_B.code(), 48);
        assert_eq!(
            to_evdev(KeyName::Letter('b')).map(evdev::Key::code),
            Some(48)
        );
        assert_eq!(Key::KEY_F10.code(), 68);
        assert_eq!(Key::KEY_F11.code(), 87);
        assert_eq!(
            to_evdev(KeyName::Function(11)).map(evdev::Key::code),
            Some(87)
        );
    }

    #[test]
    fn every_parsed_key_maps_to_a_distinct_code() {
        let mut seen = BTreeSet::new();
        for c in 'a'..='z' {
            let key = to_evdev(KeyName::Letter(c)).expect("mapped");
            assert!(seen.insert(key.code()), "{c} collided");
        }
        for c in '0'..='9' {
            let key = to_evdev(KeyName::Digit(c)).expect("mapped");
            assert!(seen.insert(key.code()), "{c} collided");
        }
        for n in 1..=12u8 {
            let key = to_evdev(KeyName::Function(n)).expect("mapped");
            assert!(seen.insert(key.code()), "f{n} collided");
        }
    }

    #[test]
    fn an_out_of_range_function_key_does_not_panic() {
        assert_eq!(to_evdev(KeyName::Function(0)), None);
        assert_eq!(to_evdev(KeyName::Function(99)), None);
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
    fn a_non_modifier_key_is_not_treated_as_one() {
        assert!(!Modifiers::default().set(Key::KEY_B, true));
    }
}

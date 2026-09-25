//! Emulating keyboard input.
//!
//! `enigo` drives the native input APIs on Windows, macOS and X11. Wayland is
//! the exception: it deliberately forbids an ordinary client from synthesising
//! input, and no library can work around that. There, a command override is the
//! supported route — typically `ydotool`, which talks to its own uinput daemon.
//!
//! Every function therefore checks for a configured override first and falls
//! back to the native backend.

use crate::config::Commands;
use crate::exec;
use std::time::Duration;

/// Headroom on top of the predicted typing duration, covering daemon startup
/// and scheduling jitter.
const TYPING_OVERHEAD: Duration = Duration::from_secs(15);

/// Timeout for a single key press run through an override command.
const KEY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum TypingError {
    #[error("could not reach the input backend: {0}")]
    Backend(String),
    #[error(transparent)]
    Command(#[from] exec::ExecError),
}

/// Turns a backend error into something a user can act on.
#[cfg(feature = "native-input")]
fn describe_backend_failure(raw: &str) -> String {
    if cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return format!(
            "{raw}. Wayland does not allow applications to synthesise input. \
             Install ydotool and set commands.type_text in the config."
        );
    }
    raw.to_string()
}

/// Types `text` into the focused window, one character at a time.
///
/// # Errors
///
/// Returns [`TypingError::Backend`] if input cannot be synthesised, or a command
/// error if a configured override failed.
pub fn type_text(text: &str, commands: &Commands, delay_ms: u32) -> Result<(), TypingError> {
    if text.is_empty() {
        return Ok(());
    }

    if let Some(argv) = commands.type_text.as_ref() {
        let argv = exec::substitute(argv, &[("delay", delay_ms.to_string())]);
        // A long, slowly typed passage legitimately takes a while; a fixed
        // timeout would cut it off and leave half the text in the window.
        let predicted = u64::from(delay_ms)
            .saturating_mul(2)
            .saturating_mul(text.chars().count() as u64);
        exec::run(
            &argv,
            Some(text.as_bytes()),
            Duration::from_millis(predicted) + TYPING_OVERHEAD,
        )?;
        return Ok(());
    }

    native_type(text, delay_ms)
}

/// Presses Return.
///
/// # Errors
///
/// Returns an error if the key could not be sent.
pub fn press_enter(commands: &Commands) -> Result<(), TypingError> {
    if let Some(argv) = commands.key_enter.as_ref() {
        exec::run(argv, None, KEY_TIMEOUT)?;
        return Ok(());
    }
    native_enter()
}

/// Presses the platform's paste chord.
///
/// # Errors
///
/// Returns an error if the keys could not be sent.
pub fn press_paste(commands: &Commands, terminal_classes: &[String]) -> Result<(), TypingError> {
    // A terminal needs a different chord. Ctrl+V there is readline's
    // quoted-insert, which takes the next input literally — so the terminal's
    // own bracketed-paste markers land in the line as `^[[200~` text with the
    // pasted URL between them, instead of being interpreted as a paste.
    let terminal =
        focused_class(commands).is_some_and(|class| is_terminal(&class, terminal_classes));

    if terminal {
        if let Some(argv) = commands.key_paste_terminal.as_ref() {
            exec::run(argv, None, KEY_TIMEOUT)?;
            return Ok(());
        }
        return native_paste_terminal();
    }

    if let Some(argv) = commands.key_paste.as_ref() {
        exec::run(argv, None, KEY_TIMEOUT)?;
        return Ok(());
    }
    native_paste()
}

/// Whether `class` names a terminal, by the configured list.
///
/// A case-insensitive substring match, so one entry covers the several names
/// a terminal goes by: `konsole` matches `org.kde.konsole` too.
#[must_use]
pub fn is_terminal(class: &str, terminal_classes: &[String]) -> bool {
    let class = class.trim().to_ascii_lowercase();
    if class.is_empty() {
        return false;
    }
    terminal_classes
        .iter()
        .any(|candidate| !candidate.is_empty() && class.contains(&candidate.to_ascii_lowercase()))
}

/// The focused window's class, if anything on this system can say.
///
/// Wayland offers no way to ask, so this shells out. `None` means the question
/// could not be answered — not that the window is not a terminal — and the
/// caller then pastes the ordinary way, which is what happened before any of
/// this existed.
fn focused_class(commands: &Commands) -> Option<String> {
    let ask = |argv: Vec<String>| -> Option<String> {
        exec::run(&argv, None, KEY_TIMEOUT)
            .ok()
            .map(|out| out.stdout_text())
            .filter(|text| !text.is_empty())
    };

    if let Some(argv) = commands.window_class.as_ref() {
        return ask(argv.clone());
    }

    // kdotool speaks to KWin over D-Bus and so works on Wayland; xdotool is
    // the X11 equivalent. Both need two steps: the window, then its class.
    if let Some(window) = ask(vec!["kdotool".to_string(), "getactivewindow".to_string()]) {
        if let Some(class) = ask(vec![
            "kdotool".to_string(),
            "getwindowclassname".to_string(),
            window,
        ]) {
            return Some(class);
        }
    }

    ask(vec![
        "xdotool".to_string(),
        "getactivewindow".to_string(),
        "getwindowclassname".to_string(),
    ])
}

#[cfg(feature = "native-input")]
mod backend {
    use super::{describe_backend_failure, TypingError};
    use std::time::Duration;

    fn open() -> Result<enigo::Enigo, TypingError> {
        enigo::Enigo::new(&enigo::Settings::default())
            .map_err(|e| TypingError::Backend(describe_backend_failure(&e.to_string())))
    }

    pub(super) fn type_text(text: &str, delay_ms: u32) -> Result<(), TypingError> {
        use enigo::Keyboard;
        let mut enigo = open()?;
        let delay = Duration::from_millis(u64::from(delay_ms));

        // Typed per character rather than in one call: many applications — chat
        // clients and terminals especially — drop characters from a burst, which
        // is the whole reason this action exists instead of pasting.
        for character in text.chars() {
            enigo
                .text(&character.to_string())
                .map_err(|e| TypingError::Backend(e.to_string()))?;
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }
        Ok(())
    }

    pub(super) fn press_enter() -> Result<(), TypingError> {
        use enigo::{Direction, Keyboard};
        open()?
            .key(enigo::Key::Return, Direction::Click)
            .map_err(|e| TypingError::Backend(e.to_string()))
    }

    pub(super) fn press_paste() -> Result<(), TypingError> {
        use enigo::{Direction, Keyboard};
        let mut enigo = open()?;
        let modifier = paste_modifier();

        let send = |enigo: &mut enigo::Enigo, key, direction| {
            enigo
                .key(key, direction)
                .map_err(|e| TypingError::Backend(e.to_string()))
        };

        send(&mut enigo, modifier, Direction::Press)?;
        let result = send(&mut enigo, enigo::Key::Unicode('v'), Direction::Click);
        // Released even if the keystroke failed, so a failure cannot leave the
        // modifier stuck down for the user's whole session.
        let release = send(&mut enigo, modifier, Direction::Release);
        result.and(release)
    }

    /// Presses the paste chord a terminal uses: the same, plus Shift.
    pub(super) fn press_paste_terminal() -> Result<(), TypingError> {
        use enigo::{Direction, Keyboard};
        let mut enigo = open()?;
        let modifier = paste_modifier();

        let send = |enigo: &mut enigo::Enigo, key, direction| {
            enigo
                .key(key, direction)
                .map_err(|e| TypingError::Backend(e.to_string()))
        };

        send(&mut enigo, modifier, Direction::Press)?;
        let shift = send(&mut enigo, enigo::Key::Shift, Direction::Press);
        let result =
            shift.and_then(|()| send(&mut enigo, enigo::Key::Unicode('v'), Direction::Click));
        // Both released whatever happened, so a failure cannot leave a
        // modifier stuck down for the rest of the session.
        let release_shift = send(&mut enigo, enigo::Key::Shift, Direction::Release);
        let release_modifier = send(&mut enigo, modifier, Direction::Release);
        result.and(release_shift).and(release_modifier)
    }

    /// The modifier used for paste on this platform.
    pub(super) const fn paste_modifier() -> enigo::Key {
        #[cfg(target_os = "macos")]
        {
            enigo::Key::Meta
        }
        #[cfg(not(target_os = "macos"))]
        {
            enigo::Key::Control
        }
    }
}

#[cfg(feature = "native-input")]
fn native_type(text: &str, delay_ms: u32) -> Result<(), TypingError> {
    backend::type_text(text, delay_ms)
}

#[cfg(feature = "native-input")]
fn native_enter() -> Result<(), TypingError> {
    backend::press_enter()
}

#[cfg(feature = "native-input")]
fn native_paste() -> Result<(), TypingError> {
    backend::press_paste()
}

#[cfg(feature = "native-input")]
fn native_paste_terminal() -> Result<(), TypingError> {
    backend::press_paste_terminal()
}

/// Built without a native backend: only a configured override can type.
#[cfg(not(feature = "native-input"))]
fn no_backend() -> TypingError {
    TypingError::Backend(
        "this build has no native input backend; set commands.type_text in the config".to_string(),
    )
}

#[cfg(not(feature = "native-input"))]
fn native_type(_text: &str, _delay_ms: u32) -> Result<(), TypingError> {
    Err(no_backend())
}

#[cfg(not(feature = "native-input"))]
fn native_enter() -> Result<(), TypingError> {
    Err(no_backend())
}

#[cfg(not(feature = "native-input"))]
fn native_paste() -> Result<(), TypingError> {
    Err(no_backend())
}

#[cfg(not(feature = "native-input"))]
fn native_paste_terminal() -> Result<(), TypingError> {
    Err(no_backend())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Typing drives real input devices, so these tests check routing and
    // argument handling rather than synthesising keystrokes, which would type
    // into whatever window happens to be focused.

    fn override_with(argv: &[&str]) -> Commands {
        Commands {
            type_text: Some(argv.iter().map(|s| (*s).to_string()).collect()),
            ..Commands::default()
        }
    }

    #[test]
    fn empty_text_is_a_no_op_that_starts_no_process() {
        // Would fail with Spawn if the override were actually run.
        let commands = override_with(&["clip-convert-no-such-program"]);
        assert!(type_text("", &commands, 12).is_ok());
    }

    #[test]
    fn a_configured_override_is_used_instead_of_the_native_backend() {
        let commands = override_with(&["true"]);
        assert!(type_text("hello", &commands, 0).is_ok());
    }

    #[test]
    fn the_key_delay_is_substituted_into_an_override() {
        let commands = override_with(&["ydotool", "type", "--key-delay", "{delay}"]);
        let argv = exec::substitute(
            commands.type_text.as_ref().expect("set"),
            &[("delay", 25.to_string())],
        );
        assert_eq!(argv, ["ydotool", "type", "--key-delay", "25"]);
    }

    #[test]
    fn text_reaches_an_override_on_stdin_verbatim() {
        let hostile = "$(id) `whoami` ; rm -rf /\nsecond line";
        let out = exec::run(
            &["cat".to_string()],
            Some(hostile.as_bytes()),
            Duration::from_secs(10),
        )
        .expect("cat runs");
        assert_eq!(String::from_utf8_lossy(&out.stdout), hostile);
    }

    #[test]
    fn a_missing_override_program_is_reported() {
        let commands = override_with(&["clip-convert-no-such-program"]);
        let err = type_text("hello", &commands, 1).expect_err("no such program");
        assert!(matches!(err, TypingError::Command(_)), "{err:?}");
    }

    #[test]
    fn terminals_are_recognised_by_substring_and_case() {
        let classes = crate::config::Config::default().terminal_classes;
        for terminal in [
            "konsole",
            "org.kde.konsole",
            "Konsole",
            "Alacritty",
            "org.wezfurlong.wezterm",
            "com.mitchellh.ghostty",
            "foot",
            "xterm-256color",
        ] {
            assert!(is_terminal(terminal, &classes), "{terminal} is a terminal");
        }

        for other in [
            "firefox",
            "com.anthropic.Claude",
            "org.kde.dolphin",
            "code",
            "",
            "   ",
        ] {
            assert!(!is_terminal(other, &classes), "{other} is not a terminal");
        }
    }

    #[test]
    fn an_empty_candidate_never_matches_everything() {
        // A stray "" in the config list would otherwise make every window a
        // terminal, since every string contains the empty string.
        assert!(!is_terminal("firefox", &[String::new()]));
    }

    #[cfg(feature = "native-input")]
    #[test]
    fn the_paste_modifier_matches_the_platform() {
        let modifier = backend::paste_modifier();
        if cfg!(target_os = "macos") {
            assert_eq!(modifier, enigo::Key::Meta);
        } else {
            assert_eq!(modifier, enigo::Key::Control);
        }
    }

    #[cfg(feature = "native-input")]
    #[test]
    fn a_wayland_backend_failure_explains_the_workaround() {
        // The bare backend error is useless on Wayland; the message has to say
        // what to do about it.
        if cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let described = describe_backend_failure("no permission");
            assert!(described.contains("ydotool"), "{described}");
            assert!(described.contains("commands.type_text"), "{described}");
        }
    }
}

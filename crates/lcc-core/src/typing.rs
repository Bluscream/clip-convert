//! Emulating keyboard input.
//!
//! Under Wayland a normal client cannot synthesise input, so this goes through
//! `ydotool`, which talks to its own uinput daemon. The commands are config
//! templates, so an X11-only setup can point them at `xdotool` instead.

use crate::config::Commands;
use crate::exec::{self, ExecError};
use std::time::Duration;

/// Headroom on top of the predicted typing duration, covering daemon startup
/// and scheduling jitter.
const TYPING_OVERHEAD: Duration = Duration::from_secs(15);

/// Timeout for a single key press.
const KEY_TIMEOUT: Duration = Duration::from_secs(10);

/// Types `text` into the focused window, one key at a time.
///
/// The text goes in on stdin rather than as an argument: it can be long, and it
/// must never be word-split or interpreted.
///
/// # Errors
///
/// Returns an [`ExecError`] if the typing tool is missing, fails, or exceeds the
/// time its own configured key delay implies.
pub fn type_text(text: &str, commands: &Commands, delay_ms: u32) -> Result<(), ExecError> {
    if text.is_empty() {
        return Ok(());
    }

    let argv = exec::substitute(&commands.type_text, &[("delay", delay_ms.to_string())]);

    // A long paste at a slow key delay legitimately takes a while; a fixed
    // timeout would cut it off partway and leave half the text in the window.
    let predicted = u64::from(delay_ms)
        .saturating_mul(2)
        .saturating_mul(text.chars().count() as u64);
    let timeout = Duration::from_millis(predicted) + TYPING_OVERHEAD;

    exec::run(&argv, Some(text.as_bytes()), timeout)?;
    Ok(())
}

/// Presses Return.
///
/// # Errors
///
/// Returns an [`ExecError`] if the key tool is missing or fails.
pub fn press_enter(commands: &Commands) -> Result<(), ExecError> {
    exec::run(&commands.key_enter, None, KEY_TIMEOUT)?;
    Ok(())
}

/// Presses the paste chord.
///
/// # Errors
///
/// Returns an [`ExecError`] if the key tool is missing or fails.
pub fn press_paste(commands: &Commands) -> Result<(), ExecError> {
    exec::run(&commands.key_paste, None, KEY_TIMEOUT)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Typing drives real input devices, so these tests check the command that
    /// would be run rather than running it — synthesising keystrokes into
    /// whatever window happens to be focused would type into the user's session.
    fn commands_capturing_with(program: &str) -> Commands {
        Commands {
            type_text: vec![
                program.to_string(),
                "type".to_string(),
                "--key-delay".to_string(),
                "{delay}".to_string(),
            ],
            ..Commands::default()
        }
    }

    #[test]
    fn the_key_delay_is_substituted_into_the_command() {
        let commands = commands_capturing_with("echo");
        let argv = exec::substitute(&commands.type_text, &[("delay", 25.to_string())]);
        assert_eq!(argv, ["echo", "type", "--key-delay", "25"]);
    }

    #[test]
    fn empty_text_is_a_no_op_that_starts_no_process() {
        // Would fail with Spawn if it tried to run.
        let commands = commands_capturing_with("lcc-no-such-program");
        assert!(type_text("", &commands, 12).is_ok());
    }

    #[test]
    fn the_text_is_delivered_on_stdin_verbatim() {
        let commands = Commands {
            type_text: vec!["cat".to_string()],
            ..Commands::default()
        };
        let argv = exec::substitute(&commands.type_text, &[("delay", "0".to_string())]);
        let hostile = "$(id) `whoami` ; rm -rf /\nsecond line";
        let out =
            exec::run(&argv, Some(hostile.as_bytes()), Duration::from_secs(10)).expect("cat runs");
        assert_eq!(String::from_utf8_lossy(&out.stdout), hostile);
    }

    #[test]
    fn a_missing_typing_tool_is_reported() {
        let commands = commands_capturing_with("lcc-no-such-program");
        let err = type_text("hello", &commands, 1).expect_err("no such program");
        assert!(matches!(err, ExecError::Spawn { .. }), "{err:?}");
    }

    #[test]
    fn the_timeout_scales_with_the_amount_of_text() {
        // 5000 characters at 20ms each cannot fit in the 15s overhead alone, so
        // a fixed timeout would truncate the text partway through.
        let chars = 5000u64;
        let delay = 20u64;
        let predicted = Duration::from_millis(delay * 2 * chars) + TYPING_OVERHEAD;
        assert!(
            predicted > Duration::from_secs(chars * delay / 1000),
            "timeout must exceed the time the typing itself will take"
        );
    }
}

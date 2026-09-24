//! Carrying out an action against clipboard content.
//!
//! This is where built-ins and user commands converge. It stays UI-free: where
//! an action needs to ask the user something, it calls through the [`Prompt`]
//! trait, which the desktop layer implements with real dialogs and tests
//! implement with canned answers.

use crate::action::{Action, Builtin, InputMode, OutputMode, Run};
use crate::clipboard;
use crate::config::Config;
use crate::content::Clip;
use crate::presets::Preset;
use crate::{exec, image, shorten, text, typing};
use std::time::Duration;

/// How long a user-configured command may run.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Questions an action may need to ask before it can proceed.
///
/// Returning `None` means the user cancelled, which is not an error.
pub trait Prompt {
    /// Asks for a character limit, pre-filled with `default`.
    fn ask_limit(&self, title: &str, message: &str, default: usize) -> Option<usize>;
    /// Asks which size target to resize to.
    fn ask_resize_target(&self, presets: &[Preset]) -> Option<Preset>;
}

/// What an action did, once it succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// A short line for the notification.
    pub message: String,
    /// Whether the clipboard now holds a new value.
    ///
    /// Drives "Paste after action": an action that already typed its result into
    /// the window must not then be pasted again.
    pub clipboard_changed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("there is nothing to type: the clipboard holds an image")]
    NotTextual,
    #[error("`{label}` does not apply to this clipboard content")]
    WrongKind { label: String },
    #[error(transparent)]
    Shorten(#[from] shorten::ShortenError),
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Clipboard(#[from] clipboard::ClipboardError),
    #[error(transparent)]
    Command(#[from] exec::ExecError),
    #[error(transparent)]
    Typing(#[from] typing::TypingError),
    #[error("could not create a temporary file: {0}")]
    TempFile(#[source] std::io::Error),
}

/// Runs `action` against `clip`.
///
/// # Errors
///
/// Returns the failure from whichever step did not complete. A cancelled prompt
/// is `Ok(None)` rather than an error.
///
/// # Panics
///
/// Does not panic; the split limit is validated at config load.
pub fn run(
    action: &Action,
    clip: &Clip,
    config: &Config,
    prompt: &dyn Prompt,
) -> Result<Option<Outcome>, RunError> {
    if !action.applies_to(clip.kind()) {
        return Err(RunError::WrongKind {
            label: action.label.clone(),
        });
    }

    match &action.run {
        Run::Builtin(builtin) => run_builtin(*builtin, clip, config, prompt),
        Run::Command {
            argv,
            input,
            output,
        } => run_command(argv, *input, *output, clip, config),
    }
}

fn run_builtin(
    builtin: Builtin,
    clip: &Clip,
    config: &Config,
    prompt: &dyn Prompt,
) -> Result<Option<Outcome>, RunError> {
    match builtin {
        Builtin::Type => {
            let body = clip.as_text().ok_or(RunError::NotTextual)?;
            typing::type_text(body, &config.commands, config.type_delay_ms)?;
            Ok(Some(Outcome {
                message: format!("Typed {} characters.", body.chars().count()),
                clipboard_changed: false,
            }))
        }
        Builtin::Shorten => run_shorten(clip, config),
        Builtin::Split => run_split(clip, config, prompt),
        Builtin::Truncate => run_truncate(clip, config, prompt),
        Builtin::Resize => run_resize(clip, config, prompt),
    }
}

fn run_shorten(clip: &Clip, config: &Config) -> Result<Option<Outcome>, RunError> {
    let Clip::Url(url) = clip else {
        return Err(RunError::WrongKind {
            label: "Shorten".to_string(),
        });
    };

    let available = config.active_shorteners();
    let chosen = shorten::pick(&available).ok_or(shorten::ShortenError::NoneConfigured)?;
    let short = shorten::shorten(url, chosen, config.ignore_ssl_errors)?;

    clipboard::write_text(short.as_str())?;
    Ok(Some(Outcome {
        message: format!("Shortened via {}: {short}", chosen.name),
        clipboard_changed: true,
    }))
}

fn run_split(
    clip: &Clip,
    config: &Config,
    prompt: &dyn Prompt,
) -> Result<Option<Outcome>, RunError> {
    let body = clip.as_text().ok_or(RunError::NotTextual)?;
    let Some(limit) = prompt.ask_limit(
        "Split",
        "Send the text in pieces of at most this many characters:",
        config.split.default_limit,
    ) else {
        return Ok(None);
    };
    if limit == 0 {
        return Ok(None);
    }

    let pieces = text::split(body, limit);
    let total = pieces.len();

    for (index, piece) in pieces.iter().enumerate() {
        typing::type_text(piece, &config.commands, config.type_delay_ms)?;
        if config.split.press_enter {
            typing::press_enter(&config.commands)?;
        }
        // The pause lets the receiving app accept each piece; chat clients in
        // particular drop messages sent back to back.
        if index + 1 < total {
            std::thread::sleep(Duration::from_millis(config.split.delay_ms));
        }
    }

    Ok(Some(Outcome {
        message: format!("Sent {total} pieces of up to {limit} characters."),
        clipboard_changed: false,
    }))
}

fn run_truncate(
    clip: &Clip,
    config: &Config,
    prompt: &dyn Prompt,
) -> Result<Option<Outcome>, RunError> {
    let body = clip.as_text().ok_or(RunError::NotTextual)?;
    let Some(limit) = prompt.ask_limit(
        "Truncate",
        "Shorten the text to at most this many characters:",
        config.truncate.default_limit,
    ) else {
        return Ok(None);
    };

    let shortened = text::truncate(body, limit, &config.truncate.ellipsis);
    clipboard::write_text(&shortened)?;

    Ok(Some(Outcome {
        message: format!("Truncated to {} characters.", shortened.chars().count()),
        clipboard_changed: true,
    }))
}

fn run_resize(
    clip: &Clip,
    config: &Config,
    prompt: &dyn Prompt,
) -> Result<Option<Outcome>, RunError> {
    let Clip::Image { bytes, .. } = clip else {
        return Err(RunError::WrongKind {
            label: "Resize".to_string(),
        });
    };

    let Some(target) = prompt.ask_resize_target(&config.presets) else {
        return Ok(None);
    };

    let resized = image::resize(bytes, &target)?;
    clipboard::write_image(&resized.mime, &resized.bytes)?;

    Ok(Some(Outcome {
        message: format!(
            "Resized to {}×{} ({}) for {}.",
            resized.width,
            resized.height,
            crate::content::human_bytes(resized.bytes.len()),
            target.label
        ),
        clipboard_changed: true,
    }))
}

fn run_command(
    argv: &[String],
    input: InputMode,
    output: OutputMode,
    clip: &Clip,
    config: &Config,
) -> Result<Option<Outcome>, RunError> {
    // Held until the command has run, so a File-mode temp file still exists.
    let mut scratch: Option<tempfile::TempDir> = None;
    let mut argv = argv.to_vec();
    let mut stdin: Option<&[u8]> = None;

    match input {
        InputMode::Stdin => stdin = Some(clip.as_bytes()),
        InputMode::Argument => {
            // Appended as one argument, so its contents cannot become more.
            argv.push(clip.as_text().ok_or(RunError::NotTextual)?.to_string());
        }
        InputMode::File => {
            let dir = tempfile::tempdir().map_err(RunError::TempFile)?;
            let path = dir.path().join("clipboard");
            std::fs::write(&path, clip.as_bytes()).map_err(RunError::TempFile)?;
            argv.push(path.display().to_string());
            scratch = Some(dir);
        }
        InputMode::None => {}
    }

    let result = exec::run(&argv, stdin, COMMAND_TIMEOUT)?;
    drop(scratch);

    match output {
        OutputMode::Discard => Ok(Some(Outcome {
            message: format!("Ran {}.", argv.first().map_or("command", String::as_str)),
            clipboard_changed: false,
        })),
        OutputMode::Notify => Ok(Some(Outcome {
            message: text::truncate(&result.stdout_text(), 300, "..."),
            clipboard_changed: false,
        })),
        OutputMode::Type => {
            let body = result.stdout_text();
            typing::type_text(&body, &config.commands, config.type_delay_ms)?;
            Ok(Some(Outcome {
                message: format!("Typed {} characters.", body.chars().count()),
                clipboard_changed: false,
            }))
        }
        OutputMode::Clipboard => {
            // A command fed an image may well answer with one. Valid UTF-8 is
            // treated as text; anything else is put back as image data.
            if let Ok(body) = std::str::from_utf8(&result.stdout) {
                let body = body.trim();
                clipboard::write_text(body)?;
                return Ok(Some(Outcome {
                    message: format!("Copied {} characters.", body.chars().count()),
                    clipboard_changed: true,
                }));
            }

            let mime = if let Clip::Image { mime, .. } = clip {
                mime.clone()
            } else {
                "image/png".to_string()
            };
            clipboard::write_image(&mime, &result.stdout)?;
            Ok(Some(Outcome {
                message: format!(
                    "Copied {}.",
                    crate::content::human_bytes(result.stdout.len())
                ),
                clipboard_changed: true,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::ActionSpec;
    use crate::content::ContentKind;

    /// Answers every prompt the same way, so a test can state its intent.
    struct Canned {
        limit: Option<usize>,
        target: Option<Preset>,
    }

    impl Prompt for Canned {
        fn ask_limit(&self, _: &str, _: &str, _: usize) -> Option<usize> {
            self.limit
        }
        fn ask_resize_target(&self, _: &[Preset]) -> Option<Preset> {
            self.target.clone()
        }
    }

    fn cancels() -> Canned {
        Canned {
            limit: None,
            target: None,
        }
    }

    fn command_action(id: &str, argv: &[&str], input: InputMode, output: OutputMode) -> Action {
        Action::from_spec(&ActionSpec {
            id: id.to_string(),
            label: id.to_string(),
            when: vec!["any".to_string()],
            builtin: None,
            command: argv.iter().map(|s| (*s).to_string()).collect(),
            input,
            output,
            enabled: true,
        })
        .expect("valid action")
    }

    fn builtin_action(which: Builtin, when: &[&str]) -> Action {
        Action::from_spec(&ActionSpec {
            id: "a".to_string(),
            label: "A".to_string(),
            when: when.iter().map(|s| (*s).to_string()).collect(),
            builtin: Some(which),
            command: Vec::new(),
            input: InputMode::default(),
            output: OutputMode::default(),
            enabled: true,
        })
        .expect("valid action")
    }

    #[test]
    fn an_action_refuses_content_it_does_not_apply_to() {
        let action = builtin_action(Builtin::Resize, &["image"]);
        let clip = Clip::Text("hello".to_string());
        let err = run(&action, &clip, &Config::default(), &cancels()).expect_err("wrong kind");
        assert!(matches!(err, RunError::WrongKind { .. }), "{err:?}");
    }

    #[test]
    fn typing_an_image_reports_that_there_is_nothing_to_type() {
        let action = builtin_action(Builtin::Type, &["any"]);
        let clip = Clip::Image {
            mime: "image/png".to_string(),
            bytes: vec![1, 2, 3],
        };
        let err = run(&action, &clip, &Config::default(), &cancels()).expect_err("no text");
        assert!(matches!(err, RunError::NotTextual), "{err:?}");
        assert!(err.to_string().contains("nothing to type"), "{err}");
    }

    #[test]
    fn cancelling_a_prompt_is_not_an_error_and_does_nothing() {
        let action = builtin_action(Builtin::Split, &["text"]);
        let clip = Clip::Text("some text".to_string());
        let outcome = run(&action, &clip, &Config::default(), &cancels()).expect("cancel is fine");
        assert_eq!(outcome, None);
    }

    #[test]
    fn shortening_without_a_configured_backend_says_so() {
        let action = builtin_action(Builtin::Shorten, &["url"]);
        let clip = crate::content::classify_text("https://example.com/x");
        assert_eq!(clip.kind(), ContentKind::Url);

        let err = run(&action, &clip, &Config::default(), &cancels()).expect_err("none configured");
        assert!(
            err.to_string().contains("no shortener is configured"),
            "{err}"
        );
    }

    #[test]
    fn a_command_receives_the_clipboard_on_stdin_and_notifies_with_its_output() {
        let action = command_action("echo", &["cat"], InputMode::Stdin, OutputMode::Notify);
        let clip = Clip::Text("payload".to_string());
        let outcome = run(&action, &clip, &Config::default(), &cancels())
            .expect("runs")
            .expect("not cancelled");
        assert_eq!(outcome.message, "payload");
        assert!(!outcome.clipboard_changed);
    }

    #[test]
    fn a_command_can_take_the_clipboard_as_a_final_argument() {
        let action = command_action("echo", &["echo"], InputMode::Argument, OutputMode::Notify);
        let clip = Clip::Text("as an argument".to_string());
        let outcome = run(&action, &clip, &Config::default(), &cancels())
            .expect("runs")
            .expect("not cancelled");
        assert_eq!(outcome.message, "as an argument");
    }

    #[test]
    fn a_command_can_take_the_clipboard_as_a_file() {
        let action = command_action("cat", &["cat"], InputMode::File, OutputMode::Notify);
        let clip = Clip::Text("via a file".to_string());
        let outcome = run(&action, &clip, &Config::default(), &cancels())
            .expect("runs")
            .expect("not cancelled");
        assert_eq!(outcome.message, "via a file");
    }

    #[test]
    fn a_command_given_no_input_still_runs() {
        let action = command_action(
            "echo",
            &["echo", "standalone"],
            InputMode::None,
            OutputMode::Notify,
        );
        let clip = Clip::Text("ignored".to_string());
        let outcome = run(&action, &clip, &Config::default(), &cancels())
            .expect("runs")
            .expect("not cancelled");
        assert_eq!(outcome.message, "standalone");
    }

    #[test]
    fn a_discarding_command_reports_that_it_ran() {
        let action = command_action("true", &["true"], InputMode::None, OutputMode::Discard);
        let clip = Clip::Text("x".to_string());
        let outcome = run(&action, &clip, &Config::default(), &cancels())
            .expect("runs")
            .expect("not cancelled");
        assert!(!outcome.clipboard_changed);
        assert!(outcome.message.contains("true"), "{}", outcome.message);
    }

    #[test]
    fn a_failing_command_surfaces_its_error_rather_than_appearing_to_succeed() {
        let action = command_action(
            "fail",
            &["sh", "-c", "echo went wrong >&2; exit 1"],
            InputMode::None,
            OutputMode::Discard,
        );
        let clip = Clip::Text("x".to_string());
        let err = run(&action, &clip, &Config::default(), &cancels()).expect_err("fails");
        assert!(err.to_string().contains("went wrong"), "{err}");
    }

    #[test]
    fn hostile_clipboard_text_reaches_a_command_verbatim() {
        let action = command_action("cat", &["cat"], InputMode::Stdin, OutputMode::Notify);
        let hostile = "$(touch /tmp/lcc-should-not-exist); rm -rf ~";
        let clip = Clip::Text(hostile.to_string());
        let outcome = run(&action, &clip, &Config::default(), &cancels())
            .expect("runs")
            .expect("not cancelled");
        assert_eq!(outcome.message, hostile);
        assert!(
            !std::path::Path::new("/tmp/lcc-should-not-exist").exists(),
            "clipboard content was interpreted by a shell"
        );
    }

    #[test]
    fn a_notified_output_is_truncated_so_it_fits_a_notification() {
        let action = command_action(
            "big",
            &["sh", "-c", "printf 'x%.0s' $(seq 1 5000)"],
            InputMode::None,
            OutputMode::Notify,
        );
        let clip = Clip::Text("x".to_string());
        let outcome = run(&action, &clip, &Config::default(), &cancels())
            .expect("runs")
            .expect("not cancelled");
        assert!(
            outcome.message.chars().count() <= 300,
            "{}",
            outcome.message.len()
        );
        assert!(outcome.message.ends_with("..."));
    }
}

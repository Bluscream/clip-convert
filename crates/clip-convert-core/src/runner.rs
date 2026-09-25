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
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Questions an action may need to ask before it can proceed.
///
/// Returning `None` means the user cancelled, which is not an error.
pub trait Prompt {
    /// Asks for a character limit, pre-filled with `default`.
    fn ask_limit(&self, title: &str, message: &str, default: usize) -> Option<usize>;
    /// Asks which size target to resize to.
    fn ask_resize_target(&self, presets: &[Preset]) -> Option<Preset>;
    /// Asks which format to convert to, given what the content already is.
    fn ask_conversion(
        &self,
        source: &str,
        options: &[crate::protocol::ActionEntry],
    ) -> Option<String>;
    /// Asks for a pattern and its replacement, offering what was used before.
    fn ask_replace(
        &self,
        patterns: &[String],
        replacements: &[String],
    ) -> Option<crate::replace::Replacement>;
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
    #[error("`{pattern}` is not a valid regular expression: {source}")]
    BadPattern {
        pattern: String,
        #[source]
        source: regex::Error,
    },
    #[error("there is nothing here that can be converted to another format")]
    NothingToConvert,
    #[error(transparent)]
    Convert(#[from] crate::convert::ConvertError),
    #[error("none of the {total} files could be processed. {first}")]
    BatchFailed { total: usize, first: String },
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
    if !action.applies_to(&clip.kinds()) {
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
            let body = clip.text().ok_or(RunError::NotTextual)?;
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
        Builtin::Replace => run_replace(clip, config, prompt),
        Builtin::Convert => run_convert(clip, config, prompt),
    }
}

fn run_shorten(clip: &Clip, config: &Config) -> Result<Option<Outcome>, RunError> {
    let url = clip.url().ok_or_else(|| RunError::WrongKind {
        label: "Shorten".to_string(),
    })?;

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
    let body = clip.text().ok_or(RunError::NotTextual)?;
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
    let body = clip.text().ok_or(RunError::NotTextual)?;
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

fn run_replace(
    clip: &Clip,
    config: &Config,
    prompt: &dyn Prompt,
) -> Result<Option<Outcome>, RunError> {
    let body = clip.text().ok_or(RunError::NotTextual)?;
    let Some(replacement) =
        prompt.ask_replace(&config.replace.patterns, &config.replace.replacements)
    else {
        return Ok(None);
    };

    let result =
        crate::replace::apply(body, &replacement).map_err(|source| RunError::BadPattern {
            pattern: replacement.pattern.clone(),
            source,
        })?;

    // A pattern that matched nothing still ends here rather than in an error:
    // the text is simply unchanged, and saying so is more useful than a
    // failure dialog.
    if result.matches == 0 {
        return Ok(Some(Outcome {
            message: format!(
                "Nothing matched `{}`; the clipboard is unchanged.",
                replacement.pattern
            ),
            clipboard_changed: false,
        }));
    }

    clipboard::write_text(&result.text)?;
    Ok(Some(Outcome {
        message: format!(
            "Replaced {} match{}.",
            result.matches,
            if result.matches == 1 { "" } else { "es" }
        ),
        clipboard_changed: true,
    }))
}

fn run_resize(
    clip: &Clip,
    config: &Config,
    prompt: &dyn Prompt,
) -> Result<Option<Outcome>, RunError> {
    let Some(target) = prompt.ask_resize_target(&config.presets) else {
        return Ok(None);
    };

    // Image data held directly on the clipboard is replaced in place. A
    // selection of image files is a batch, and its results are written out as
    // files because a clipboard can only hold one image at a time.
    if let Some((_, bytes)) = clip.image() {
        return resize_one(bytes, &target).map(Some);
    }

    let files = clip.files().ok_or_else(|| RunError::WrongKind {
        label: "Resize".to_string(),
    })?;

    let batch = crate::batch::resize_batch(files, &target)?;
    clipboard::write_files(&batch.written)?;
    Ok(Some(batch.into_outcome(&target)))
}

/// Asks what to convert to, then does it.
///
/// The choice is a second dialog rather than a longer action menu: which
/// conversions exist depends on what the clipboard holds, and folding them
/// into the first menu would make its length depend on the content in a way
/// that makes the common entries move around.
fn run_convert(
    clip: &Clip,
    config: &Config,
    prompt: &dyn Prompt,
) -> Result<Option<Outcome>, RunError> {
    let sources = crate::convert::source_formats(clip);
    let options = crate::convert::applicable(&config.conversions, &sources);
    if options.is_empty() {
        return Err(RunError::NothingToConvert);
    }

    let entries: Vec<crate::protocol::ActionEntry> = options
        .iter()
        .map(|c| crate::protocol::ActionEntry {
            id: c.id.clone(),
            label: c.display_label(),
            icon: c.icon.clone(),
            button_color: c.button_color.clone(),
            text_color: c.text_color.clone(),
        })
        .collect();

    let source = sources
        .iter()
        .map(|f| f.to_uppercase())
        .collect::<Vec<_>>()
        .join(", ");
    let Some(chosen) = prompt.ask_conversion(&source, &entries) else {
        return Ok(None);
    };
    let Some(conversion) = options.into_iter().find(|c| c.id == chosen) else {
        log::warn!("chosen conversion {chosen} vanished");
        return Ok(None);
    };

    convert_clip(clip, conversion)
}

/// Carries out one conversion against whatever the clipboard holds.
fn convert_clip(
    clip: &Clip,
    conversion: &crate::convert::Conversion,
) -> Result<Option<Outcome>, RunError> {
    // Rich text first: an HTML clipboard usually carries a plain-text facet
    // too, and converting the markup is what was asked for.
    if conversion.is_native() && crate::convert::normalise(&conversion.to) == "md" {
        let html = clip.html().ok_or(RunError::NothingToConvert)?;
        let markdown = crate::convert::html_to_markdown(html);
        clipboard::write_text(&markdown)?;
        return Ok(Some(Outcome {
            message: format!("Converted to Markdown ({} characters).", markdown.len()),
            clipboard_changed: true,
        }));
    }

    if let Some((_, bytes)) = clip.image() {
        let converted = crate::convert::image_to(bytes, &conversion.to)?;
        let mime = format!("image/{}", crate::convert::normalise(&conversion.to));
        clipboard::write_image(&mime, &converted)?;
        return Ok(Some(Outcome {
            message: format!(
                "Converted to {} ({}).",
                conversion.to.to_uppercase(),
                crate::content::human_bytes(converted.len())
            ),
            clipboard_changed: true,
        }));
    }

    let files = clip.files().ok_or(RunError::NothingToConvert)?;
    let batch = crate::batch::convert_batch(files, conversion)?;
    clipboard::write_files(&batch.written)?;
    Ok(Some(batch.into_conversion_outcome(conversion)))
}

/// Replaces the clipboard's image with a resized copy.
fn resize_one(bytes: &[u8], target: &Preset) -> Result<Outcome, RunError> {
    let resized = image::resize(bytes, target)?;
    clipboard::write_image(&resized.mime, &resized.bytes)?;

    Ok(Outcome {
        message: format!(
            "Resized to {}×{} ({}) for {}.",
            resized.width,
            resized.height,
            crate::content::human_bytes(resized.bytes.len()),
            target.label
        ),
        clipboard_changed: true,
    })
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
            argv.push(clip.text().ok_or(RunError::NotTextual)?.to_string());
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

            let mime = clip
                .image()
                .map_or_else(|| "image/png".to_string(), |(mime, _)| mime.to_string());
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
    use std::path::PathBuf;

    /// Answers every prompt the same way, so a test can state its intent.
    struct Canned {
        limit: Option<usize>,
        target: Option<Preset>,
        replacement: Option<crate::replace::Replacement>,
        conversion: Option<String>,
    }

    impl Prompt for Canned {
        fn ask_limit(&self, _: &str, _: &str, _: usize) -> Option<usize> {
            self.limit
        }
        fn ask_resize_target(&self, _: &[Preset]) -> Option<Preset> {
            self.target.clone()
        }
        fn ask_replace(&self, _: &[String], _: &[String]) -> Option<crate::replace::Replacement> {
            self.replacement.clone()
        }
        fn ask_conversion(&self, _: &str, _: &[crate::protocol::ActionEntry]) -> Option<String> {
            self.conversion.clone()
        }
    }

    fn cancels() -> Canned {
        Canned {
            limit: None,
            target: None,
            replacement: None,
            conversion: None,
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
            icon: None,
            button_color: None,
            text_color: None,
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
            icon: None,
            button_color: None,
            text_color: None,
        })
        .expect("valid action")
    }

    /// Answers the resize prompt with a fixed target.
    fn with_target(preset: Preset) -> Canned {
        Canned {
            limit: None,
            target: Some(preset),
            replacement: None,
            conversion: None,
        }
    }

    /// Answers the replace prompt with a fixed pattern.
    fn with_replacement(pattern: &str, with: &str) -> Canned {
        Canned {
            limit: None,
            target: None,
            replacement: Some(crate::replace::Replacement {
                pattern: pattern.to_string(),
                replacement: with.to_string(),
            }),
            conversion: None,
        }
    }

    fn small_png(path: &std::path::Path, width: u32, height: u32) {
        let mut img = ::image::RgbaImage::new(width, height);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let v = u8::try_from((x * 13 + y * 7) % 256).unwrap_or(0);
            *pixel = ::image::Rgba([v, v, v, 255]);
        }
        let bytes = crate::encode::encode(
            &::image::DynamicImage::ImageRgba8(img),
            crate::encode::Format::Png,
            95,
        )
        .expect("encodes");
        std::fs::write(path, bytes).expect("writes");
    }

    fn preset() -> Preset {
        Preset {
            id: "t".to_string(),
            label: "Test target".to_string(),
            width: 32,
            height: 32,
            max_bytes: 0,
            format: Some("png".to_string()),
            fit: crate::presets::Fit::Inside,
            icon: None,
            button_color: None,
            text_color: None,
        }
    }

    #[test]
    fn resizing_a_file_selection_writes_every_result() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sources: Vec<PathBuf> = (0..3)
            .map(|i| {
                let path = dir.path().join(format!("shot{i}.png"));
                small_png(&path, 100, 80);
                path
            })
            .collect();

        let batch = crate::batch::resize_batch(&sources, &preset()).expect("batch runs");
        assert_eq!(batch.written.len(), 3);
        assert!(batch.failures.is_empty());
        for path in &batch.written {
            let decoded = ::image::open(path).expect("a readable image");
            assert!(decoded.width() <= 32 && decoded.height() <= 32);
        }

        let message = batch.into_outcome(&preset()).message;
        assert!(message.contains("Resized 3 of 3"), "{message}");
    }

    #[test]
    fn a_batch_reports_the_files_it_could_not_process_without_abandoning_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let good = dir.path().join("good.png");
        small_png(&good, 60, 60);
        let bad = dir.path().join("broken.png");
        std::fs::write(&bad, b"not an image").expect("writes");

        let batch = crate::batch::resize_batch(&[good, bad], &preset())
            .expect("partial success still works");
        assert_eq!(batch.written.len(), 1);
        let message = batch.into_outcome(&preset()).message;
        assert!(message.contains("Resized 1 of 2"), "{message}");
        assert!(message.contains("broken.png"), "{message}");
    }

    #[test]
    fn a_batch_where_everything_fails_is_an_error_not_a_silent_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bad = dir.path().join("broken.png");
        std::fs::write(&bad, b"not an image").expect("writes");

        let err = crate::batch::resize_batch(&[bad], &preset()).expect_err("nothing succeeded");
        assert!(
            matches!(err, RunError::BatchFailed { total: 1, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn resize_is_offered_for_image_files_and_uses_the_batch_path() {
        // A file selection contributes the image kind, so the menu offers
        // Resize; it must then actually work rather than report a wrong kind.
        let clip = Clip::from_files(vec![PathBuf::from("/a/one.png")]).expect("non-empty");
        let action = builtin_action(Builtin::Resize, &["image"]);
        assert!(action.applies_to(&clip.kinds()));

        // Cancelling at the prompt proves it reached the resize path rather
        // than being rejected for the wrong kind.
        let outcome = run(&action, &clip, &Config::default(), &cancels()).expect("reaches prompt");
        assert_eq!(outcome, None);
    }

    #[test]
    fn resizing_text_is_still_refused() {
        let clip = Clip::from_text("just words").expect("non-empty");
        let action = builtin_action(Builtin::Resize, &["image"]);
        let err = run(&action, &clip, &Config::default(), &with_target(preset()))
            .expect_err("text has no image");
        assert!(matches!(err, RunError::WrongKind { .. }), "{err:?}");
    }

    #[test]
    fn an_action_refuses_content_it_does_not_apply_to() {
        let action = builtin_action(Builtin::Resize, &["image"]);
        let clip = Clip::from_text("hello").expect("non-empty");
        let err = run(&action, &clip, &Config::default(), &cancels()).expect_err("wrong kind");
        assert!(matches!(err, RunError::WrongKind { .. }), "{err:?}");
    }

    #[test]
    fn typing_an_image_reports_that_there_is_nothing_to_type() {
        let action = builtin_action(Builtin::Type, &["any"]);
        let clip = Clip::from_image("image/png".to_string(), vec![1, 2, 3]).expect("non-empty");
        let err = run(&action, &clip, &Config::default(), &cancels()).expect_err("no text");
        assert!(matches!(err, RunError::NotTextual), "{err:?}");
        assert!(err.to_string().contains("nothing to type"), "{err}");
    }

    #[test]
    fn cancelling_a_prompt_is_not_an_error_and_does_nothing() {
        let action = builtin_action(Builtin::Split, &["text"]);
        let clip = Clip::from_text("some text").expect("non-empty");
        let outcome = run(&action, &clip, &Config::default(), &cancels()).expect("cancel is fine");
        assert_eq!(outcome, None);
    }

    #[test]
    fn shortening_without_a_configured_backend_says_so() {
        let action = builtin_action(Builtin::Shorten, &["url"]);
        let clip = Clip::from_text("https://example.com/x").expect("non-empty");
        assert!(clip.kinds().contains(&ContentKind::Url));

        let err = run(&action, &clip, &Config::default(), &cancels()).expect_err("none configured");
        assert!(
            err.to_string().contains("no shortener is configured"),
            "{err}"
        );
    }

    #[test]
    fn a_command_receives_the_clipboard_on_stdin_and_notifies_with_its_output() {
        let action = command_action("echo", &["cat"], InputMode::Stdin, OutputMode::Notify);
        let clip = Clip::from_text("payload").expect("non-empty");
        let outcome = run(&action, &clip, &Config::default(), &cancels())
            .expect("runs")
            .expect("not cancelled");
        assert_eq!(outcome.message, "payload");
        assert!(!outcome.clipboard_changed);
    }

    #[test]
    fn a_command_can_take_the_clipboard_as_a_final_argument() {
        let action = command_action("echo", &["echo"], InputMode::Argument, OutputMode::Notify);
        let clip = Clip::from_text("as an argument").expect("non-empty");
        let outcome = run(&action, &clip, &Config::default(), &cancels())
            .expect("runs")
            .expect("not cancelled");
        assert_eq!(outcome.message, "as an argument");
    }

    #[test]
    fn a_command_can_take_the_clipboard_as_a_file() {
        let action = command_action("cat", &["cat"], InputMode::File, OutputMode::Notify);
        let clip = Clip::from_text("via a file").expect("non-empty");
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
        let clip = Clip::from_text("ignored").expect("non-empty");
        let outcome = run(&action, &clip, &Config::default(), &cancels())
            .expect("runs")
            .expect("not cancelled");
        assert_eq!(outcome.message, "standalone");
    }

    #[test]
    fn a_discarding_command_reports_that_it_ran() {
        let action = command_action("true", &["true"], InputMode::None, OutputMode::Discard);
        let clip = Clip::from_text("x").expect("non-empty");
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
        let clip = Clip::from_text("x").expect("non-empty");
        let err = run(&action, &clip, &Config::default(), &cancels()).expect_err("fails");
        assert!(err.to_string().contains("went wrong"), "{err}");
    }

    #[test]
    fn hostile_clipboard_text_reaches_a_command_verbatim() {
        let action = command_action("cat", &["cat"], InputMode::Stdin, OutputMode::Notify);
        let hostile = "$(touch /tmp/lcc-should-not-exist); rm -rf ~";
        let clip = Clip::from_text(hostile).expect("non-empty");
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
        let clip = Clip::from_text("x").expect("non-empty");
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

    #[test]
    fn replace_that_matches_nothing_leaves_the_clipboard_alone() {
        let clip = Clip::from_text("nothing to see").expect("non-empty");
        let outcome = run(
            &builtin_action(Builtin::Replace, &["text"]),
            &clip,
            &Config::default(),
            &with_replacement("zzz+", "x"),
        )
        .expect("runs")
        .expect("an outcome");

        assert!(!outcome.clipboard_changed, "{}", outcome.message);
        assert!(
            outcome.message.contains("Nothing matched"),
            "{}",
            outcome.message
        );
    }

    #[test]
    fn a_pattern_that_does_not_compile_is_reported_with_the_pattern() {
        let clip = Clip::from_text("anything").expect("non-empty");
        let err = run(
            &builtin_action(Builtin::Replace, &["text"]),
            &clip,
            &Config::default(),
            &with_replacement("([", "x"),
        )
        .expect_err("a broken pattern must be an error");

        match err {
            RunError::BadPattern { pattern, .. } => assert_eq!(pattern, "(["),
            other => panic!("expected BadPattern, got {other}"),
        }
    }

    #[test]
    fn a_cancelled_replace_does_nothing_at_all() {
        let clip = Clip::from_text("a-b").expect("non-empty");
        let outcome = run(
            &builtin_action(Builtin::Replace, &["text"]),
            &clip,
            &Config::default(),
            &cancels(),
        )
        .expect("cancelling is not a failure");
        assert_eq!(outcome, None);
    }

    fn conversion(id: &str, to: &str, command: &[&str]) -> crate::convert::Conversion {
        crate::convert::Conversion {
            id: id.to_string(),
            label: None,
            from: vec!["png".to_string()],
            to: to.to_string(),
            command: command.iter().map(|c| (*c).to_string()).collect(),
            enabled: true,
            icon: None,
            button_color: None,
            text_color: None,
        }
    }

    #[test]
    fn a_native_conversion_writes_a_file_in_the_target_format() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("shot.png");
        small_png(&source, 32, 32);

        let out =
            crate::batch::convert_file(&source, &conversion("to-ico", "ico", &[]), dir.path())
                .expect("converts");
        assert_eq!(out.extension().and_then(|e| e.to_str()), Some("ico"));
        let bytes = std::fs::read(&out).expect("readable");
        assert_eq!(
            ::image::guess_format(&bytes).expect("a known format"),
            ::image::ImageFormat::Ico
        );
    }

    #[test]
    fn a_command_that_prints_its_result_has_it_captured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("note.png");
        std::fs::write(&source, b"ignored").expect("write");

        // No {output} in the command, so stdout becomes the file.
        let out = crate::batch::convert_file(
            &source,
            &conversion("to-txt", "txt", &["/bin/echo", "hello {input}"]),
            dir.path(),
        )
        .expect("runs");
        let written = std::fs::read_to_string(&out).expect("readable");
        assert!(written.starts_with("hello "), "{written}");
        assert!(written.contains("note.png"), "{written}");
    }

    #[test]
    fn a_command_given_an_output_path_is_trusted_to_write_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("note.png");
        std::fs::write(&source, b"contents").expect("write");

        let out = crate::batch::convert_file(
            &source,
            &conversion("to-txt", "txt", &["/bin/cp", "{input}", "{output}"]),
            dir.path(),
        )
        .expect("runs");
        assert_eq!(
            std::fs::read(&out).expect("readable"),
            b"contents",
            "the command's own output file must be left alone"
        );
    }

    #[test]
    fn a_batch_skips_files_the_conversion_does_not_take() {
        let dir = tempfile::tempdir().expect("tempdir");
        let png = dir.path().join("a.png");
        small_png(&png, 8, 8);
        let other = dir.path().join("b.txt");
        std::fs::write(&other, b"not an image").expect("write");

        let batch = crate::batch::convert_batch(&[png, other], &conversion("to-gif", "gif", &[]))
            .expect("the png alone is enough");
        assert_eq!(batch.total, 1, "only the png should have been considered");
        assert_eq!(batch.written.len(), 1);
        assert!(batch.failures.is_empty());
    }
}

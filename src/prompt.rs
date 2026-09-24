//! Asking the user something, by running the dialog process.
//!
//! The daemon holds no graphical toolkit. When an action needs an answer it
//! starts `clip-convert-dialog`, hands it one JSON request on stdin and reads
//! one JSON reply back. The dialog exists only while it is on screen.
//!
//! This is what makes the idle cost of sitting in the tray essentially nothing,
//! and it is also the only way to have no window on Wayland, where a window
//! cannot be hidden once created.

use anyhow::{Context, Result};
use clipconv::exec;
use clipconv::presets::Preset;
use clipconv::protocol::{ActionChoice, ActionEntry, Answer, Ask, Reply, Request, WindowSize};
use clipconv::runner::Prompt;
use std::sync::Arc;
use std::time::Duration;

/// How long a dialog may stay open before it is abandoned.
///
/// Generous, because it is waiting for a person. The limit exists only so that a
/// dialog left open forever cannot block every later hotkey press.
const DIALOG_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The name of the dialog program, which sits beside this one.
const HELPER: &str = if cfg!(target_os = "windows") {
    "clip-convert-dialog.exe"
} else {
    "clip-convert-dialog"
};

/// Remembers how big each dialog was left.
///
/// A trait rather than a direct dependency on the application, so the dialog
/// plumbing can be tested without a config file on disk.
pub trait SizeStore: Send + Sync {
    /// The size this dialog was last left at.
    fn size(&self, key: &str) -> Option<WindowSize>;
    /// Records the size a dialog was closed at.
    fn set_size(&self, key: &str, size: WindowSize);
}

/// Runs dialogs in a separate process.
#[derive(Clone)]
pub struct Prompter {
    /// The program and arguments to run. A vector rather than a bare path so a
    /// test can point it at a stand-in without writing an executable to disk.
    command: Vec<String>,
    sizes: Arc<dyn SizeStore>,
}

impl Prompter {
    /// Locates the dialog program.
    ///
    /// # Errors
    ///
    /// Returns an error if it is not installed beside this executable, which is
    /// worth failing loudly for: without it the hotkey can do nothing.
    pub fn new(sizes: Arc<dyn SizeStore>) -> Result<Self> {
        let mut path = std::env::current_exe().context("locating this executable")?;
        path.pop();
        let helper = path.join(HELPER);

        if !helper.is_file() {
            anyhow::bail!(
                "{HELPER} was not found next to this program (looked in {}). \
                 It is required to show any dialog.",
                path.display()
            );
        }
        Ok(Self {
            command: vec![helper.display().to_string()],
            sizes,
        })
    }

    /// Shows `request` and waits for the reply.
    ///
    /// Returns [`Reply::Cancelled`] for anything that went wrong, so a failure
    /// to show a dialog behaves like the user dismissing it rather than leaving
    /// the action half-done.
    fn ask(&self, request: &Request) -> Reply {
        let key = request.size_key();
        let ask = Ask {
            request: request.clone(),
            size: self.sizes.size(key),
        };

        let encoded = match serde_json::to_string(&ask) {
            Ok(encoded) => encoded,
            Err(e) => {
                log::error!("could not encode the dialog request: {e}");
                return Reply::Cancelled;
            }
        };

        let output = match exec::run(&self.command, Some(encoded.as_bytes()), DIALOG_TIMEOUT) {
            Ok(output) => output,
            Err(e) => {
                log::error!("the dialog process failed: {e}");
                return Reply::Cancelled;
            }
        };

        let body = output.stdout_text();
        if body.is_empty() {
            log::debug!("the dialog closed without answering");
            return Reply::Cancelled;
        }

        let answer: Answer = match serde_json::from_str(&body) {
            Ok(answer) => answer,
            Err(e) => {
                log::error!("could not read the dialog reply: {e}");
                return Reply::Cancelled;
            }
        };

        // Remembered even when the dialog was dismissed: someone who resizes a
        // window and then presses Escape still meant the new size.
        if let Some(size) = answer.size.filter(|s| s.is_usable()) {
            if self.sizes.size(key) != Some(size) {
                self.sizes.set_size(key, size);
            }
        }

        answer.reply
    }

    /// Shows the action menu for the current clipboard content.
    #[must_use]
    pub fn choose_action(
        &self,
        description: &str,
        actions: &[ActionEntry],
        paste_after: bool,
    ) -> Option<ActionChoice> {
        match self.ask(&Request::ChooseAction {
            description: description.to_string(),
            actions: actions.to_vec(),
            paste_after,
        }) {
            Reply::Action { choice } => Some(choice),
            _ => None,
        }
    }

    /// Reports a failure to the user.
    pub fn error(&self, message: &str) {
        let _ = self.ask(&Request::ShowError {
            message: message.to_string(),
        });
    }
}

impl Prompt for Prompter {
    fn ask_limit(&self, title: &str, message: &str, default: usize) -> Option<usize> {
        match self.ask(&Request::AskLimit {
            title: title.to_string(),
            message: message.to_string(),
            default,
        }) {
            Reply::Limit { value } => Some(value),
            _ => None,
        }
    }

    fn ask_resize_target(&self, presets: &[Preset]) -> Option<Preset> {
        match self.ask(&Request::AskResizeTarget {
            presets: presets.to_vec(),
        }) {
            Reply::ResizeTarget { preset } => Some(*preset),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prompter pointed at a stand-in command, so the plumbing can be tested
    /// without opening a window.
    ///
    /// The script is passed to `sh -c` rather than written to a file and
    /// executed. Writing an executable and running it from a multithreaded test
    /// binary races: another thread's still-open write descriptor is inherited
    /// by the fork, and the exec fails with ETXTBSY.
    /// Records sizes in memory, so nothing touches a config file.
    #[derive(Default)]
    struct Remembered {
        sizes: std::sync::Mutex<std::collections::BTreeMap<String, WindowSize>>,
    }

    impl SizeStore for Remembered {
        fn size(&self, key: &str) -> Option<WindowSize> {
            self.sizes.lock().ok()?.get(key).copied()
        }
        fn set_size(&self, key: &str, size: WindowSize) {
            if let Ok(mut sizes) = self.sizes.lock() {
                sizes.insert(key.to_string(), size);
            }
        }
    }

    fn stub(script: &str) -> Prompter {
        stub_with(script, Arc::new(Remembered::default()))
    }

    fn stub_with(script: &str, sizes: Arc<dyn SizeStore>) -> Prompter {
        Prompter {
            command: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
            sizes,
        }
    }

    #[test]
    fn a_reply_is_read_back() {
        let prompter = stub(r#"echo '{"reply":{"reply":"limit","value":140}}'"#);
        assert_eq!(prompter.ask_limit("t", "m", 2000), Some(140));
    }

    #[test]
    fn the_request_reaches_the_dialog_on_stdin() {
        // The stub reads stdin and answers with it as an error message, so the
        // request comes back out through the normal reply path.
        let prompter = stub(
            r#"body=$(cat); printf '{"reply":"show_error_unused","x":"%s"}' "$body" >/dev/null; \
               case "$body" in *ask_limit*) echo '{"reply":{"reply":"limit","value":1}}';; \
               *) echo '{"reply":{"reply":"cancelled"}}';; esac"#,
        );
        assert_eq!(
            prompter.ask_limit("Split", "How many?", 2000),
            Some(1),
            "the helper should have seen an ask_limit request on stdin"
        );
    }

    #[test]
    fn a_cancelled_dialog_yields_nothing() {
        let prompter = stub(r#"echo '{"reply":{"reply":"cancelled"}}'"#);
        assert_eq!(prompter.ask_limit("t", "m", 1), None);
        assert_eq!(prompter.ask_resize_target(&[]), None);
        assert_eq!(prompter.choose_action("d", &[], true), None);
    }

    #[test]
    fn a_dialog_that_prints_nothing_is_treated_as_cancelled() {
        let prompter = stub("exit 0");
        assert_eq!(prompter.ask_limit("t", "m", 1), None);
    }

    #[test]
    fn a_dialog_that_prints_rubbish_is_treated_as_cancelled() {
        // Must not panic or hang: whatever the helper does, the action ends.
        let prompter = stub("echo not json at all");
        assert_eq!(prompter.ask_limit("t", "m", 1), None);
    }

    #[test]
    fn a_dialog_that_fails_is_treated_as_cancelled() {
        let prompter = stub("echo broken >&2; exit 3");
        assert_eq!(prompter.ask_limit("t", "m", 1), None);
    }

    #[test]
    fn an_action_choice_carries_the_checkbox_back() {
        let prompter = stub(
            r#"echo '{"reply":{"reply":"action","choice":{"action_id":"truncate","paste_after":false}}}'"#,
        );
        let choice = prompter
            .choose_action(
                "d",
                &[ActionEntry {
                    id: "truncate".to_string(),
                    label: "Truncate".to_string(),
                    icon: None,
                }],
                true,
            )
            .expect("a choice");
        assert_eq!(choice.action_id, "truncate");
        assert!(!choice.paste_after, "the unchecked box must come back");
    }

    #[test]
    fn the_size_a_dialog_was_left_at_is_remembered_and_sent_back_next_time() {
        let sizes: Arc<Remembered> = Arc::new(Remembered::default());
        let store: Arc<dyn SizeStore> = sizes.clone();

        // First run: the dialog reports the size it closed at.
        let prompter = stub_with(
            r#"cat >/dev/null; echo '{"reply":{"reply":"limit","value":1},"size":{"width":640,"height":480}}'"#,
            store.clone(),
        );
        assert_eq!(prompter.ask_limit("t", "m", 1), Some(1));
        assert_eq!(
            sizes.size("limit"),
            Some(WindowSize {
                width: 640,
                height: 480
            })
        );

        // Second run: that size is sent back out in the request.
        let echoing = stub_with(
            r#"body=$(cat); case "$body" in *640*) echo '{"reply":{"reply":"limit","value":2}}';; *) echo '{"reply":{"reply":"cancelled"}}';; esac"#,
            store,
        );
        assert_eq!(
            echoing.ask_limit("t", "m", 1),
            Some(2),
            "the remembered size should have been sent with the request"
        );
    }

    #[test]
    fn an_unusable_size_is_not_remembered() {
        let sizes: Arc<Remembered> = Arc::new(Remembered::default());
        let prompter = stub_with(
            r#"cat >/dev/null; echo '{"reply":{"reply":"cancelled"},"size":{"width":4,"height":4}}'"#,
            sizes.clone(),
        );
        let _ = prompter.ask_limit("t", "m", 1);
        assert_eq!(sizes.size("limit"), None, "a sliver must not be stored");
    }

    #[test]
    fn a_dialog_dismissed_after_a_resize_still_records_the_size() {
        // Someone who resizes a window then presses Escape still meant it.
        let sizes: Arc<Remembered> = Arc::new(Remembered::default());
        let prompter = stub_with(
            r#"cat >/dev/null; echo '{"reply":{"reply":"cancelled"},"size":{"width":700,"height":500}}'"#,
            sizes.clone(),
        );
        assert_eq!(prompter.ask_limit("t", "m", 1), None);
        assert_eq!(
            sizes.size("limit"),
            Some(WindowSize {
                width: 700,
                height: 500
            })
        );
    }

    #[test]
    fn a_missing_dialog_program_is_treated_as_cancelled_not_a_crash() {
        let prompter = Prompter {
            command: vec!["clip-convert-no-such-dialog".to_string()],
            sizes: Arc::new(Remembered::default()),
        };
        assert_eq!(prompter.ask_limit("t", "m", 1), None);
    }
}

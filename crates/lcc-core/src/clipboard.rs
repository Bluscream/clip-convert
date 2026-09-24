//! Reading, writing and watching the Wayland clipboard via `wl-clipboard`.
//!
//! Change notification is event-driven: `wl-paste --watch` runs a command each
//! time the selection changes, so a single long-lived child process emits one
//! line per change and the app blocks on a channel in between. Polling the
//! clipboard on a timer — as many tray tools do — spawns a process several times
//! a second forever and still adds latency, which is exactly the idle cost this
//! app is meant not to have.

use crate::content::{self, Clip};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// How long a single clipboard read or write may take.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// The marker `wl-paste --watch` is asked to echo on each change. Its content is
/// irrelevant; only the fact that a line arrived matters.
const CHANGE_MARKER: &str = "changed";

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("the clipboard is empty")]
    Empty,
    #[error("clipboard command failed: {0}")]
    Command(#[from] crate::exec::ExecError),
    #[error("could not start the clipboard watcher: {0}")]
    Watch(#[source] std::io::Error),
}

/// Lists the MIME types the current selection is offered as.
///
/// # Errors
///
/// Returns [`ClipboardError::Empty`] when nothing is on the clipboard.
pub fn offered_types() -> Result<Vec<String>, ClipboardError> {
    let argv = ["wl-paste", "--list-types"].map(String::from);
    match crate::exec::run(&argv, None, IO_TIMEOUT) {
        Ok(out) => {
            let types: Vec<String> = out
                .stdout_text()
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            if types.is_empty() {
                Err(ClipboardError::Empty)
            } else {
                Ok(types)
            }
        }
        // wl-paste exits non-zero with "No selection" for an empty clipboard,
        // which is an ordinary state rather than a failure worth reporting.
        Err(crate::exec::ExecError::Failed { .. }) => Err(ClipboardError::Empty),
        Err(other) => Err(other.into()),
    }
}

/// Reads the current selection and classifies it.
///
/// # Errors
///
/// Returns [`ClipboardError::Empty`] for an empty clipboard, or a command error
/// if `wl-paste` is missing or fails.
pub fn read() -> Result<Clip, ClipboardError> {
    let types = offered_types()?;

    if let Some(mime) = content::pick_image_mime(&types) {
        let argv = ["wl-paste", "--no-newline", "--type", &mime].map(String::from);
        let out = crate::exec::run(&argv, None, IO_TIMEOUT)?;
        if out.stdout.is_empty() {
            return Err(ClipboardError::Empty);
        }
        return Ok(Clip::Image {
            mime,
            bytes: out.stdout,
        });
    }

    let argv = ["wl-paste", "--no-newline"].map(String::from);
    let out = crate::exec::run(&argv, None, IO_TIMEOUT)?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return Err(ClipboardError::Empty);
    }

    Ok(content::classify_text(&text))
}

/// Puts text on the clipboard.
///
/// # Errors
///
/// Returns a command error if `wl-copy` is missing or fails.
pub fn write_text(text: &str) -> Result<(), ClipboardError> {
    let argv = ["wl-copy", "--type", "text/plain;charset=utf-8"].map(String::from);
    crate::exec::run(&argv, Some(text.as_bytes()), IO_TIMEOUT)?;
    Ok(())
}

/// Puts image data on the clipboard under the given MIME type.
///
/// # Errors
///
/// Returns a command error if `wl-copy` is missing or fails.
pub fn write_image(mime: &str, bytes: &[u8]) -> Result<(), ClipboardError> {
    let argv = ["wl-copy", "--type", mime].map(String::from);
    crate::exec::run(&argv, Some(bytes), IO_TIMEOUT)?;
    Ok(())
}

/// A running `wl-paste --watch` child that reports selection changes.
///
/// Killing the child on drop matters: without it, a crash or restart of the app
/// would leave orphaned watchers behind, each holding a Wayland connection.
pub struct Watcher {
    child: Child,
    changes: Receiver<()>,
}

impl Watcher {
    /// Starts watching the clipboard.
    ///
    /// # Errors
    ///
    /// Returns [`ClipboardError::Watch`] if `wl-paste` could not be started.
    pub fn start() -> Result<Self, ClipboardError> {
        let mut child = Command::new("wl-paste")
            .args(["--watch", "echo", CHANGE_MARKER])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(ClipboardError::Watch)?;

        let stdout = child.stdout.take().ok_or_else(|| {
            ClipboardError::Watch(std::io::Error::other("wl-paste gave no stdout"))
        })?;

        let (tx, changes) = mpsc::channel();
        std::thread::spawn(move || {
            // Blocks in read until the compositor reports a change: no timer, no
            // wakeups, nothing scheduled while the clipboard is untouched.
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(_) => {
                        if tx.send(()).is_err() {
                            break; // The app has gone away.
                        }
                    }
                    Err(e) => {
                        log::warn!("clipboard watcher stopped reading: {e}");
                        break;
                    }
                }
            }
            log::debug!("clipboard watcher thread finished");
        });

        Ok(Self { child, changes })
    }

    /// The channel that receives one message per clipboard change.
    #[must_use]
    pub fn changes(&self) -> &Receiver<()> {
        &self.changes
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The clipboard is live user data. These tests deliberately exercise only
    // the parsing and process plumbing, never a read or write of the real
    // selection, which would clobber whatever the user had copied.

    #[test]
    fn the_watcher_starts_and_is_cleaned_up_on_drop() {
        let Ok(watcher) = Watcher::start() else {
            // No Wayland display in this environment; nothing to assert.
            return;
        };
        let pid = watcher.child.id();
        assert!(
            std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "watcher should be running"
        );

        drop(watcher);
        std::thread::sleep(Duration::from_millis(200));

        let still_alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            // A reaped child may linger briefly as a zombie; that is still cleaned up.
            .is_some_and(|stat| !stat.contains(") Z "));
        assert!(!still_alive, "watcher child outlived its Watcher");
    }

    #[test]
    fn an_empty_type_list_reads_as_an_empty_clipboard() {
        // Mirrors what offered_types does with wl-paste's output, without
        // touching the real selection.
        let raw = "  \n\n   \n";
        let types: Vec<String> = raw
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        assert!(types.is_empty());
    }

    #[test]
    fn type_lists_are_trimmed_and_blank_lines_dropped() {
        let raw = "text/plain\n\n image/png \ntext/html\n";
        let types: Vec<String> = raw
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(types, ["text/plain", "image/png", "text/html"]);
        assert_eq!(
            content::pick_image_mime(&types).as_deref(),
            Some("image/png")
        );
    }
}

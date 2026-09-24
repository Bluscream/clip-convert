//! Reading, writing and watching the system clipboard.
//!
//! Reading and writing go through `arboard`, which wraps each platform's native
//! clipboard API, so no external helper program is needed anywhere.
//!
//! Watching is the part that cannot be made uniform. Wayland offers no
//! clipboard-change event to an ordinary client, but `wl-paste --watch` can
//! subscribe on our behalf, which costs nothing while the clipboard is idle.
//! Where no such event source exists the watcher falls back to polling, which is
//! stated plainly rather than hidden: it is a fallback, not the design.
//!
//! `arboard` exchanges images as raw RGBA. Encoded PNG is kept as the internal
//! representation instead, because that is what a user's custom command expects
//! to receive on stdin and what the resize pipeline reads; conversion happens
//! here, at the boundary.

use crate::content::{self, Clip};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// How often the fallback watcher checks for a change.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long to wait for a clipboard write to start being served.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("the clipboard is empty")]
    Empty,
    #[error("the clipboard could not be reached: {0}")]
    Backend(#[from] arboard::Error),
    #[error("the clipboard image could not be converted: {0}")]
    Image(#[source] image::ImageError),
    #[error("the clipboard did not become available")]
    Unavailable,
    #[error("could not start the clipboard watcher: {0}")]
    Watch(#[source] std::io::Error),
}

/// Opens the system clipboard.
fn open() -> Result<arboard::Clipboard, ClipboardError> {
    Ok(arboard::Clipboard::new()?)
}

/// Reads the current clipboard contents and classifies them.
///
/// Images are preferred over text: an application that offers both is usually
/// offering a filename or a URL alongside the actual picture.
///
/// # Errors
///
/// Returns [`ClipboardError::Empty`] when there is nothing usable on the
/// clipboard.
pub fn read() -> Result<Clip, ClipboardError> {
    let mut clipboard = open()?;

    if let Ok(image) = clipboard.get_image() {
        return encode_clipboard_image(&image);
    }

    match clipboard.get_text() {
        Ok(text) if !text.trim().is_empty() => Ok(content::classify_text(&text)),
        _ => Err(ClipboardError::Empty),
    }
}

/// Turns the platform's raw RGBA into the PNG this app passes around.
fn encode_clipboard_image(image: &arboard::ImageData<'_>) -> Result<Clip, ClipboardError> {
    let width = u32::try_from(image.width).unwrap_or(0);
    let height = u32::try_from(image.height).unwrap_or(0);

    // A zero-sized buffer is a valid RgbaImage, so it would otherwise encode
    // happily into a 0x0 PNG that nothing downstream can use.
    if width == 0 || height == 0 {
        return Err(ClipboardError::Empty);
    }

    let buffer = image::RgbaImage::from_raw(width, height, image.bytes.to_vec())
        .ok_or(ClipboardError::Empty)?;

    // Quality 95 keeps the PNG truecolour: this is an intermediate
    // representation, so it must not lose anything before the user has chosen
    // what to do with it.
    let bytes = crate::encode::encode(
        &image::DynamicImage::ImageRgba8(buffer),
        crate::encode::Format::Png,
        95,
    )
    .map_err(|e| ClipboardError::Image(image_error(&e)))?;

    Ok(Clip::Image {
        mime: "image/png".to_string(),
        bytes,
    })
}

/// Bridges an encode failure into the error type this module reports.
fn image_error(error: &crate::encode::EncodeError) -> image::ImageError {
    image::ImageError::Encoding(image::error::EncodingError::new(
        image::error::ImageFormatHint::Unknown,
        error.to_string(),
    ))
}

/// Puts text on the clipboard.
///
/// # Errors
///
/// Returns a backend error if the clipboard could not be written.
pub fn write_text(text: &str) -> Result<(), ClipboardError> {
    let owned = text.to_string();
    let characters = text.chars().count();
    serve(move |clipboard| set_text(clipboard, owned))?;
    log::debug!("wrote {characters} characters to the clipboard");
    Ok(())
}

/// Puts an encoded image on the clipboard.
///
/// # Errors
///
/// Returns [`ClipboardError::Image`] if the bytes are not a readable image, or
/// a backend error if the clipboard could not be written.
pub fn write_image(_mime: &str, bytes: &[u8]) -> Result<(), ClipboardError> {
    let rgba = image::load_from_memory(bytes)
        .map_err(ClipboardError::Image)?
        .to_rgba8();
    let (width, height) = (rgba.width() as usize, rgba.height() as usize);
    let raw = rgba.into_raw();

    serve(move |clipboard| {
        set_image(
            clipboard,
            arboard::ImageData {
                width,
                height,
                bytes: std::borrow::Cow::Owned(raw),
            },
        )
    })?;
    log::debug!("wrote a {width}x{height} image to the clipboard");
    Ok(())
}

/// Hands `write` a clipboard handle and keeps it alive long enough to matter.
///
/// On X11 and Wayland the clipboard holds no data of its own: the application
/// that copied something stays running and hands it over on request. Writing
/// through a handle that is dropped immediately therefore publishes an offer
/// that dies at once — on KDE the result was the clipboard reverting to a
/// `application/x-kde-onlyReplaceEmpty` placeholder.
///
/// So the write happens on a thread of its own, which goes on serving the
/// selection until another application takes ownership and then exits. At most
/// one of these is alive at a time, because the next copy — by this app or any
/// other — releases the previous one.
fn serve(
    write: impl FnOnce(&mut arboard::Clipboard) -> Result<(), arboard::Error> + Send + 'static,
) -> Result<(), ClipboardError> {
    let (ready_tx, ready_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(clipboard) => clipboard,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };

        // Reported before the call, because on Linux it does not return until
        // ownership is lost, which may be minutes or hours later.
        let _ = ready_tx.send(Ok(()));

        if let Err(e) = write(&mut clipboard) {
            log::warn!("serving the clipboard ended: {e}");
        }
    });

    match ready_rx.recv_timeout(WRITE_TIMEOUT) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(ClipboardError::Backend(e)),
        Err(_) => Err(ClipboardError::Unavailable),
    }
}

/// Publishes text, keeping ownership for as long as the platform requires.
#[cfg(target_os = "linux")]
fn set_text(clipboard: &mut arboard::Clipboard, text: String) -> Result<(), arboard::Error> {
    use arboard::SetExtLinux;
    clipboard.set().wait().text(text)
}

#[cfg(not(target_os = "linux"))]
fn set_text(clipboard: &mut arboard::Clipboard, text: String) -> Result<(), arboard::Error> {
    // Windows and macOS copy the data into a system-owned clipboard, so the
    // writer does not have to stay alive.
    clipboard.set_text(text)
}

/// Publishes an image, keeping ownership for as long as the platform requires.
#[cfg(target_os = "linux")]
fn set_image(
    clipboard: &mut arboard::Clipboard,
    image: arboard::ImageData<'static>,
) -> Result<(), arboard::Error> {
    use arboard::SetExtLinux;
    clipboard.set().wait().image(image)
}

#[cfg(not(target_os = "linux"))]
fn set_image(
    clipboard: &mut arboard::Clipboard,
    image: arboard::ImageData<'static>,
) -> Result<(), arboard::Error> {
    clipboard.set_image(image)
}

/// Keeps a clipboard watcher running.
///
/// Held by the caller rather than by the thread that consumes the events: a
/// detached thread's destructors never run when the process exits, which left
/// the `wl-paste` child orphaned on every shutdown.
pub struct Watcher {
    /// The `wl-paste --watch` child, when that backend is in use. Killed on drop
    /// so a restart does not leave orphans holding a compositor connection.
    child: Option<std::process::Child>,
    /// Which backend was selected, for the startup log.
    backend: &'static str,
}

impl Watcher {
    /// Starts watching the clipboard, choosing the best backend available.
    ///
    /// Returns the watcher, which must be kept alive for as long as events are
    /// wanted, and the channel those events arrive on.
    ///
    /// # Errors
    ///
    /// Returns [`ClipboardError::Watch`] if no backend could be started.
    pub fn start() -> Result<(Self, Receiver<()>), ClipboardError> {
        #[cfg(target_os = "linux")]
        if let Some(started) = wayland::start() {
            log::info!("clipboard backend: wl-paste --watch (event-driven)");
            return Ok(started);
        }

        log::info!("clipboard backend: polling every {POLL_INTERVAL:?}");
        Ok(start_polling())
    }

    /// Which backend is in use, for diagnostics.
    #[must_use]
    pub fn backend(&self) -> &'static str {
        self.backend
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The fallback: compare the clipboard against what was last seen.
///
/// Used where the platform offers no change event. Only a hash is retained, so a
/// large image on the clipboard is not held in memory between checks.
fn start_polling() -> (Watcher, Receiver<()>) {
    let (tx, changes) = mpsc::channel();

    std::thread::spawn(move || {
        let mut last = None;
        loop {
            std::thread::sleep(POLL_INTERVAL);
            let current = fingerprint();
            if current.is_some() && current != last {
                last = current;
                if tx.send(()).is_err() {
                    break;
                }
            } else if current.is_some() {
                last = current;
            }
        }
    });

    (
        Watcher {
            child: None,
            backend: "polling",
        },
        changes,
    )
}

/// A cheap value that changes when the clipboard changes.
fn fingerprint() -> Option<u64> {
    use std::hash::{Hash, Hasher};

    let mut clipboard = arboard::Clipboard::new().ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();

    if let Ok(text) = clipboard.get_text() {
        text.hash(&mut hasher);
        return Some(hasher.finish());
    }
    if let Ok(image) = clipboard.get_image() {
        image.width.hash(&mut hasher);
        image.height.hash(&mut hasher);
        image.bytes.hash(&mut hasher);
        return Some(hasher.finish());
    }
    None
}

#[cfg(target_os = "linux")]
mod wayland {
    use super::{ClipboardError, Watcher};
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    /// The marker `wl-paste --watch` echoes on each change. Its content is
    /// irrelevant; only the arrival of a line matters.
    const CHANGE_MARKER: &str = "changed";

    /// Starts the event-driven backend, or `None` if it is not available.
    pub(super) fn start() -> Option<(Watcher, std::sync::mpsc::Receiver<()>)> {
        // Only meaningful under Wayland; X11 sessions use the polling fallback.
        std::env::var_os("WAYLAND_DISPLAY")?;

        let mut child = Command::new("wl-paste")
            .args(["--watch", "echo", CHANGE_MARKER])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| log::debug!("wl-paste is unavailable: {e}"))
            .ok()?;

        let stdout = child.stdout.take()?;
        let (tx, changes) = mpsc::channel();

        std::thread::spawn(move || {
            // Blocks in read until the compositor reports a change: no timer and
            // no wakeups while the clipboard is untouched.
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(_) => {
                        if tx.send(()).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        log::warn!("clipboard watcher stopped reading: {e}");
                        break;
                    }
                }
            }
        });

        Some((
            Watcher {
                child: Some(child),
                backend: "wl-paste",
            },
            changes,
        ))
    }

    /// Kept so the error type is used on every platform.
    #[allow(dead_code)]
    fn _assert_error_is_constructible(e: std::io::Error) -> ClipboardError {
        ClipboardError::Watch(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The clipboard is live user data. These tests exercise conversion and the
    // watcher's process handling, never a read or write of the real selection,
    // which would clobber whatever the user had copied.

    #[test]
    fn raw_rgba_from_the_clipboard_becomes_a_usable_png() {
        let pixels: Vec<u8> = (0..4u32 * 8 * 8)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        let data = arboard::ImageData {
            width: 8,
            height: 8,
            bytes: std::borrow::Cow::Owned(pixels),
        };

        let clip = encode_clipboard_image(&data).expect("converts");
        match clip {
            Clip::Image {
                ref mime,
                ref bytes,
            } => {
                assert_eq!(mime, "image/png");
                let decoded = image::load_from_memory(bytes).expect("valid png");
                assert_eq!((decoded.width(), decoded.height()), (8, 8));
            }
            other => panic!("expected an image, got {other:?}"),
        }
    }

    #[test]
    fn a_zero_sized_clipboard_image_is_treated_as_empty() {
        let data = arboard::ImageData {
            width: 0,
            height: 0,
            bytes: std::borrow::Cow::Owned(Vec::new()),
        };
        assert!(matches!(
            encode_clipboard_image(&data),
            Err(ClipboardError::Empty)
        ));
    }

    #[test]
    fn a_truncated_clipboard_image_is_rejected_rather_than_panicking() {
        // Fewer bytes than width * height * 4.
        let data = arboard::ImageData {
            width: 100,
            height: 100,
            bytes: std::borrow::Cow::Owned(vec![0; 16]),
        };
        assert!(matches!(
            encode_clipboard_image(&data),
            Err(ClipboardError::Empty)
        ));
    }

    #[test]
    fn write_image_rejects_bytes_that_are_not_an_image() {
        let err = write_image("image/png", b"definitely not a png").expect_err("not an image");
        assert!(matches!(err, ClipboardError::Image(_)), "{err:?}");
    }

    #[test]
    fn the_watcher_starts_and_is_cleaned_up_on_drop() {
        let Ok((watcher, _changes)) = Watcher::start() else {
            return;
        };
        let pid = watcher.child.as_ref().map(std::process::Child::id);
        assert!(!watcher.backend().is_empty());

        drop(watcher);

        if let Some(pid) = pid {
            std::thread::sleep(Duration::from_millis(200));
            let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .is_some_and(|stat| !stat.contains(") Z "));
            assert!(!alive, "watcher child outlived its Watcher");
        }
    }
}

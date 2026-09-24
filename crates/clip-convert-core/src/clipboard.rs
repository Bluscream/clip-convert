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

use crate::content::{Clip, Facet};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// How often the fallback watcher checks for a change.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The MIME type a file manager publishes a selection as.
const URI_LIST: &str = "text/uri-list";

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
    #[error("could not put files on the clipboard: {0}")]
    Files(String),
    #[error("putting files on the clipboard is not supported on this platform yet")]
    NoFileSupport,
    #[error("could not start the clipboard watcher: {0}")]
    Watch(#[source] std::io::Error),
}

/// Opens the system clipboard.
fn open() -> Result<arboard::Clipboard, ClipboardError> {
    Ok(arboard::Clipboard::new()?)
}

/// Reads the current clipboard contents and classifies them.
///
/// Facets are gathered in preference order. Files come first, because a file
/// manager publishes only `text/uri-list` and nothing else. Images come next,
/// and any text or HTML alongside them is kept as a further facet rather than
/// discarded — copying an image from Discord offers `image/png` together with
/// an `<img src="…">`, and both are worth acting on.
///
/// # Errors
///
/// Returns [`ClipboardError::Empty`] when there is nothing usable on the
/// clipboard.
pub fn read() -> Result<Clip, ClipboardError> {
    if let Some(clip) = read_files() {
        return Ok(clip);
    }

    let mut clipboard = open()?;

    let mut clip = match clipboard.get_image() {
        Ok(image) => encode_clipboard_image(&image)?,
        Err(_) => None,
    };

    // Text is read whether or not an image was found: it may be a caption, a
    // source URL, or the real content.
    if let Ok(text) = clipboard.get_text() {
        match clip.as_mut() {
            Some(clip) => {
                if let Some(extra) = Clip::from_text(&text) {
                    for facet in extra.facets() {
                        clip.push(facet.clone());
                    }
                }
            }
            None => clip = Clip::from_text(&text),
        }
    }

    // A single `<img src="…">` is how several chat clients describe an image
    // they have just put on the clipboard, and that source URL is worth
    // offering even though the image is the primary content.
    if let Some(clip) = clip.as_mut() {
        if let Some(url) = read_html().as_deref().and_then(source_url) {
            clip.push(Facet::Url(Box::new(url)));
        }
    }

    clip.ok_or(ClipboardError::Empty)
}

/// Extracts the source URL from an HTML fragment that is just one image.
///
/// Deliberately narrow: anything more than a single image tag is a document,
/// not a reference to one thing, and guessing which of its links was meant
/// would be wrong.
fn source_url(html: &str) -> Option<url::Url> {
    let trimmed = html.trim();
    if !trimmed.starts_with("<img") || trimmed.matches("<img").count() != 1 {
        return None;
    }

    let rest = trimmed.split_once("src=\"")?.1;
    let candidate = rest.split_once('"')?.0;
    let url = url::Url::parse(candidate).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

/// Reads `text/html`, where the platform can.
#[cfg(target_os = "linux")]
fn read_html() -> Option<String> {
    read_mime("text/html")
}

#[cfg(not(target_os = "linux"))]
fn read_html() -> Option<String> {
    None
}

/// Reads a file selection, if the clipboard holds one.
///
/// Returns `None` where the platform's file-list format is not implemented,
/// which is currently everywhere but Linux.
#[cfg(target_os = "linux")]
fn read_files() -> Option<Clip> {
    let body = read_mime(URI_LIST)?;
    let paths = crate::files::parse_uri_list(&body);
    if paths.is_empty() {
        return None;
    }
    log::debug!("clipboard holds {} file(s)", paths.len());
    Clip::from_files(paths)
}

#[cfg(not(target_os = "linux"))]
fn read_files() -> Option<Clip> {
    // Windows uses CF_HDROP and macOS NSFilenamesPboardType; neither is wired
    // up yet, so a file selection falls through to text or image.
    None
}

/// Reads one specific MIME type.
///
/// `arboard` handles only text and images, and `text/uri-list` is the sole
/// thing a file manager publishes, so this goes to the protocol directly.
#[cfg(target_os = "linux")]
fn read_mime(mime: &str) -> Option<String> {
    use wl_clipboard_rs::paste::{get_contents, ClipboardType, MimeType, Seat};

    let (mut reader, _) = get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        MimeType::Specific(mime),
    )
    .ok()?;

    let mut body = String::new();
    std::io::Read::read_to_string(&mut reader, &mut body).ok()?;
    (!body.trim().is_empty()).then_some(body)
}

/// Turns the platform's raw RGBA into the PNG this app passes around.
///
/// Returns `Ok(None)` for an unusable buffer, which is an ordinary state rather
/// than a failure.
fn encode_clipboard_image(image: &arboard::ImageData<'_>) -> Result<Option<Clip>, ClipboardError> {
    let width = u32::try_from(image.width).unwrap_or(0);
    let height = u32::try_from(image.height).unwrap_or(0);

    // A zero-sized buffer is a valid RgbaImage, so it would otherwise encode
    // happily into a 0x0 PNG that nothing downstream can use.
    if width == 0 || height == 0 {
        return Ok(None);
    }

    let Some(buffer) = image::RgbaImage::from_raw(width, height, image.bytes.to_vec()) else {
        return Ok(None);
    };

    // Quality 95 keeps the PNG truecolour: this is an intermediate
    // representation, so it must not lose anything before the user has chosen
    // what to do with it.
    let bytes = crate::encode::encode(
        &image::DynamicImage::ImageRgba8(buffer),
        crate::encode::Format::Png,
        95,
    )
    .map_err(|e| ClipboardError::Image(image_error(&e)))?;

    Ok(Clip::from_image("image/png".to_string(), bytes))
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

/// Puts a file selection on the clipboard, so it can be pasted into a file
/// manager or any application that accepts dropped files.
///
/// # Errors
///
/// Returns [`ClipboardError::NoFileSupport`] on platforms where publishing a
/// file list is not implemented.
#[cfg(target_os = "linux")]
pub fn write_files(paths: &[std::path::PathBuf]) -> Result<(), ClipboardError> {
    use wl_clipboard_rs::copy::{copy, MimeType, Options, ServeRequests, Source};

    let body = crate::files::to_uri_list(paths);
    if body.is_empty() {
        return Err(ClipboardError::Empty);
    }

    let mut options = Options::new();
    // Served until something else takes the clipboard, from a forked child, so
    // the selection survives this function returning.
    options.serve_requests(ServeRequests::Unlimited);
    options.foreground(false);

    copy(
        options,
        Source::Bytes(body.into_bytes().into()),
        MimeType::Specific(URI_LIST.to_string()),
    )
    .map_err(|e| ClipboardError::Files(e.to_string()))?;

    log::debug!("wrote {} file(s) to the clipboard", paths.len());
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn write_files(_paths: &[std::path::PathBuf]) -> Result<(), ClipboardError> {
    Err(ClipboardError::NoFileSupport)
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

        let clip = encode_clipboard_image(&data)
            .expect("converts")
            .expect("not empty");
        let (mime, bytes) = clip.image().expect("an image facet");
        assert_eq!(mime, "image/png");
        let decoded = image::load_from_memory(bytes).expect("valid png");
        assert_eq!((decoded.width(), decoded.height()), (8, 8));
    }

    #[test]
    fn a_zero_sized_clipboard_image_is_treated_as_empty() {
        let data = arboard::ImageData {
            width: 0,
            height: 0,
            bytes: std::borrow::Cow::Owned(Vec::new()),
        };
        assert!(matches!(encode_clipboard_image(&data), Ok(None)));
    }

    #[test]
    fn a_truncated_clipboard_image_is_rejected_rather_than_panicking() {
        // Fewer bytes than width * height * 4.
        let data = arboard::ImageData {
            width: 100,
            height: 100,
            bytes: std::borrow::Cow::Owned(vec![0; 16]),
        };
        assert!(matches!(encode_clipboard_image(&data), Ok(None)));
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

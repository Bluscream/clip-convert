//! What is on the clipboard, and how to describe it.
//!
//! Detection is kept free of I/O so it can be tested directly; [`crate::clipboard`]
//! does the reading and hands the bytes here.

use std::fmt;
use url::Url;

/// The category of clipboard content an action can be offered for.
///
/// This is the closed set that an action's `when` list is matched against, so it
/// is an enum rather than a bare string: a typo in a config file should be
/// reported at load time, not silently match nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ContentKind {
    /// Text that is a single, complete http(s) URL.
    Url,
    /// Any other text.
    Text,
    /// Image data.
    Image,
}

impl ContentKind {
    /// The name used in config files.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Text => "text",
            Self::Image => "image",
        }
    }

    /// Parses a config-file name. `None` for anything unrecognised.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "url" => Some(Self::Url),
            "text" => Some(Self::Text),
            "image" => Some(Self::Image),
            _ => None,
        }
    }

    /// Every kind, for reporting valid values in an error message.
    #[must_use]
    pub fn all() -> [Self; 3] {
        [Self::Url, Self::Text, Self::Image]
    }
}

impl fmt::Display for ContentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Clipboard content, already read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clip {
    Url(Box<Url>),
    Text(String),
    Image { mime: String, bytes: Vec<u8> },
}

impl Clip {
    #[must_use]
    pub fn kind(&self) -> ContentKind {
        match self {
            Self::Url(_) => ContentKind::Url,
            Self::Text(_) => ContentKind::Text,
            Self::Image { .. } => ContentKind::Image,
        }
    }

    /// The content as text, for actions that type or transform it.
    ///
    /// `None` for images, which have no meaningful text form.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Url(url) => Some(url.as_str()),
            Self::Text(text) => Some(text),
            Self::Image { .. } => None,
        }
    }

    /// The raw bytes to hand to an external command on stdin.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Url(url) => url.as_str().as_bytes(),
            Self::Text(text) => text.as_bytes(),
            Self::Image { bytes, .. } => bytes,
        }
    }

    /// A one-line summary shown at the top of the action dialog, so the user can
    /// confirm what they are about to act on without pasting it somewhere first.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Url(url) => {
                let host = url.host_str().unwrap_or("unknown host");
                format!(
                    "URL · {host} · {} characters",
                    thousands(url.as_str().chars().count())
                )
            }
            Self::Text(text) => {
                let chars = text.chars().count();
                let lines = text.lines().count();
                if lines > 1 {
                    format!(
                        "Text · {} characters · {} lines",
                        thousands(chars),
                        thousands(lines)
                    )
                } else {
                    format!("Text · {} characters", thousands(chars))
                }
            }
            Self::Image { mime, bytes } => {
                let format = mime.rsplit('/').next().unwrap_or(mime).to_uppercase();
                let size = human_bytes(bytes.len());
                match imagesize::blob_size(bytes) {
                    Ok(dim) => format!("Image · {format} · {}×{} · {size}", dim.width, dim.height),
                    // Dimensions are a nicety; an unreadable header should not
                    // stop the dialog from opening.
                    Err(_) => format!("Image · {format} · {size}"),
                }
            }
        }
    }
}

/// Classifies clipboard text as either a URL or plain text.
///
/// Only a single, complete http(s) URL counts: a sentence that happens to contain
/// a link is text, because the URL actions would have nothing unambiguous to act on.
#[must_use]
pub fn classify_text(text: &str) -> Clip {
    let trimmed = text.trim();

    if !trimmed.is_empty() && !trimmed.chars().any(char::is_whitespace) {
        if let Ok(url) = Url::parse(trimmed) {
            if matches!(url.scheme(), "http" | "https") && url.has_host() {
                return Clip::Url(Box::new(url));
            }
        }
    }

    Clip::Text(trimmed.to_string())
}

/// Chooses which offered image type to read, preferring lossless and widely
/// supported formats so that a re-encode does not compound quality loss.
#[must_use]
pub fn pick_image_mime(offered: &[String]) -> Option<String> {
    const PREFERRED: [&str; 4] = ["image/png", "image/webp", "image/jpeg", "image/gif"];

    for wanted in PREFERRED {
        if let Some(found) = offered.iter().find(|m| m.eq_ignore_ascii_case(wanted)) {
            return Some(found.clone());
        }
    }

    offered
        .iter()
        .find(|m| m.to_ascii_lowercase().starts_with("image/"))
        .cloned()
}

/// Formats a byte count the way a file manager would.
#[must_use]
pub fn human_bytes(bytes: usize) -> String {
    #[allow(clippy::cast_precision_loss)] // Display only; precision beyond 1 decimal is not shown.
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", value / 1024.0)
    } else {
        format!("{:.1} MB", value / (1024.0 * 1024.0))
    }
}

/// Groups digits so long character counts stay readable at a glance.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_url_is_a_url() {
        assert_eq!(
            classify_text("https://example.com/a").kind(),
            ContentKind::Url
        );
        assert_eq!(classify_text("http://example.com").kind(), ContentKind::Url);
    }

    #[test]
    fn surrounding_whitespace_does_not_stop_url_detection() {
        assert_eq!(
            classify_text("  \n https://example.com \t ").kind(),
            ContentKind::Url
        );
    }

    #[test]
    fn a_sentence_containing_a_url_is_text() {
        assert_eq!(
            classify_text("see https://example.com for more").kind(),
            ContentKind::Text
        );
    }

    #[test]
    fn non_web_schemes_are_text() {
        for input in [
            "file:///etc/passwd",
            "mailto:a@b.com",
            "ftp://example.com",
            "javascript:alert(1)",
        ] {
            assert_eq!(
                classify_text(input).kind(),
                ContentKind::Text,
                "{input} should not be treated as a shortenable URL"
            );
        }
    }

    #[test]
    fn single_label_hosts_are_urls() {
        // `http://localhost/` and intranet short names are legitimate targets, so
        // the host is not required to contain a dot. Note that the URL spec
        // normalises `https:///path` to host `path` rather than rejecting it.
        assert_eq!(
            classify_text("http://localhost:8080/x").kind(),
            ContentKind::Url
        );
        assert_eq!(classify_text("https:///path").kind(), ContentKind::Url);
    }

    #[test]
    fn empty_input_is_text_not_url() {
        assert_eq!(classify_text("").kind(), ContentKind::Text);
        assert_eq!(classify_text("   ").kind(), ContentKind::Text);
    }

    #[test]
    fn classified_text_is_trimmed() {
        assert_eq!(classify_text("  hi  "), Clip::Text("hi".to_string()));
    }

    #[test]
    fn png_is_preferred_over_jpeg() {
        let offered = vec!["image/jpeg".to_string(), "image/png".to_string()];
        assert_eq!(pick_image_mime(&offered).as_deref(), Some("image/png"));
    }

    #[test]
    fn an_unlisted_image_type_is_still_accepted() {
        let offered = vec!["text/html".to_string(), "image/tiff".to_string()];
        assert_eq!(pick_image_mime(&offered).as_deref(), Some("image/tiff"));
    }

    #[test]
    fn mime_matching_ignores_case() {
        let offered = vec!["IMAGE/PNG".to_string()];
        assert_eq!(pick_image_mime(&offered).as_deref(), Some("IMAGE/PNG"));
    }

    #[test]
    fn no_image_type_offered() {
        let offered = vec!["text/plain".to_string(), "text/html".to_string()];
        assert_eq!(pick_image_mime(&offered), None);
    }

    #[test]
    fn content_kind_round_trips_through_its_config_name() {
        for kind in ContentKind::all() {
            assert_eq!(ContentKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ContentKind::parse("  Image "), Some(ContentKind::Image));
        assert_eq!(ContentKind::parse("video"), None);
    }

    #[test]
    fn descriptions_name_the_kind_and_size() {
        assert!(classify_text("https://example.com/x")
            .describe()
            .starts_with("URL · example.com · "));
        assert!(classify_text("hello").describe().contains("5 characters"));
        assert!(classify_text("a\nb").describe().contains("2 lines"));
    }

    #[test]
    fn image_description_falls_back_when_the_header_is_unreadable() {
        let clip = Clip::Image {
            mime: "image/png".to_string(),
            bytes: vec![0; 10],
        };
        let described = clip.describe();
        assert!(described.starts_with("Image · PNG · "), "{described}");
    }

    #[test]
    fn thousands_groups_long_numbers() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_204), "1 204");
        assert_eq!(thousands(1_000_000), "1 000 000");
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
    }
}

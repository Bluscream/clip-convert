//! What is on the clipboard, and how to describe it.
//!
//! A clipboard does not hold one thing. It offers the same selection in several
//! representations at once, and which ones are present is what makes an action
//! applicable. Copying files in Dolphin offers `text/uri-list` and nothing else;
//! copying an image in a browser offers `image/png` alongside `text/html` and
//! the source URL as text.
//!
//! So a [`Clip`] is a list of [`Facet`]s rather than a single value. Actions
//! declare which [`ContentKind`]s they apply to, which controls whether they are
//! *offered*; when one runs it asks for the facet it actually wants. That keeps
//! "should this appear in the menu" and "what does it operate on" separate,
//! which is what lets a file selection be treated as files by one action and as
//! text by another.
//!
//! Detection is free of I/O so it can be tested directly; [`crate::clipboard`]
//! does the reading and hands the pieces here.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use url::Url;

/// The category of clipboard content an action can be offered for.
///
/// A closed set that an action's `when` list is matched against, so it is an
/// enum rather than a bare string: a typo in a config file should be reported at
/// load time, not silently match nothing.
///
/// The order is the preference order used when several facets could satisfy the
/// same request: most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ContentKind {
    /// One or more local files.
    Files,
    /// A file selection whose members are all video.
    Video,
    /// Image data, either held directly or as a selection of image files.
    Image,
    /// Rich text.
    Html,
    /// Text that is a single, complete http(s) URL.
    Url,
    /// Any other text.
    Text,
}

impl ContentKind {
    /// The name used in config files.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Video => "video",
            Self::Image => "image",
            Self::Html => "html",
            Self::Url => "url",
            Self::Text => "text",
        }
    }

    /// Parses a config-file name. `None` for anything unrecognised.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "files" | "file" => Some(Self::Files),
            "video" => Some(Self::Video),
            "image" => Some(Self::Image),
            "html" => Some(Self::Html),
            "url" => Some(Self::Url),
            "text" => Some(Self::Text),
            _ => None,
        }
    }

    /// Every kind, for reporting valid values in an error message.
    #[must_use]
    pub fn all() -> [Self; 6] {
        [
            Self::Files,
            Self::Video,
            Self::Image,
            Self::Html,
            Self::Url,
            Self::Text,
        ]
    }
}

impl fmt::Display for ContentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One representation of the clipboard's contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Facet {
    /// Local files, from `text/uri-list`.
    Files(Vec<PathBuf>),
    /// Encoded image data.
    Image {
        mime: String,
        bytes: Vec<u8>,
    },
    Html(String),
    Url(Box<Url>),
    Text(String),
}

impl Facet {
    /// Which kinds this facet makes available.
    ///
    /// A file selection contributes `Image` or `Video` as well as `Files` when
    /// its members are all of that type, so an image action can be offered for
    /// copied image files without every file action also appearing for a
    /// pasted screenshot.
    #[must_use]
    pub fn kinds(&self) -> Vec<ContentKind> {
        match self {
            Self::Files(paths) => {
                let mut kinds = vec![ContentKind::Files];
                if crate::files::all_images(paths) {
                    kinds.push(ContentKind::Image);
                } else if crate::files::all_videos(paths) {
                    kinds.push(ContentKind::Video);
                }
                kinds
            }
            Self::Image { .. } => vec![ContentKind::Image],
            Self::Html(_) => vec![ContentKind::Html],
            Self::Url(_) => vec![ContentKind::Url],
            Self::Text(_) => vec![ContentKind::Text],
        }
    }
}

/// Clipboard content, already read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    /// In preference order: the first is what an action gets when it does not
    /// care which representation it receives.
    facets: Vec<Facet>,
}

impl Clip {
    /// Builds a clip from facets in preference order.
    ///
    /// Returns `None` for an empty list, which is the same as an empty
    /// clipboard.
    #[must_use]
    pub fn new(facets: Vec<Facet>) -> Option<Self> {
        (!facets.is_empty()).then_some(Self { facets })
    }

    /// Classifies clipboard text as either a URL or plain text.
    ///
    /// Only a single, complete http(s) URL counts: a sentence that happens to
    /// contain a link is text, because the URL actions would have nothing
    /// unambiguous to act on.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }

        if !trimmed.chars().any(char::is_whitespace) {
            if let Ok(url) = Url::parse(trimmed) {
                if matches!(url.scheme(), "http" | "https") && url.has_host() {
                    return Self::new(vec![
                        Facet::Url(Box::new(url)),
                        Facet::Text(trimmed.to_string()),
                    ]);
                }
            }
        }

        Self::new(vec![Facet::Text(trimmed.to_string())])
    }

    /// Builds a clip from a file selection.
    ///
    /// A text facet is synthesised from the paths. File managers do not offer
    /// one — Dolphin publishes `text/uri-list` and nothing else — so without
    /// this there would be no way to act on a selection as text, which is
    /// exactly what "paste the paths somewhere" needs.
    #[must_use]
    pub fn from_files(paths: Vec<PathBuf>) -> Option<Self> {
        if paths.is_empty() {
            return None;
        }
        let as_text = paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        Self::new(vec![Facet::Files(paths), Facet::Text(as_text)])
    }

    /// Builds a clip from image data.
    #[must_use]
    pub fn from_image(mime: String, bytes: Vec<u8>) -> Option<Self> {
        (!bytes.is_empty()).then(|| Self {
            facets: vec![Facet::Image { mime, bytes }],
        })
    }

    /// Adds a facet at the end, if it is not already represented.
    pub fn push(&mut self, facet: Facet) {
        if !self.facets.iter().any(|f| f.kinds() == facet.kinds()) {
            self.facets.push(facet);
        }
    }

    /// Every kind this content can be acted on as.
    #[must_use]
    pub fn kinds(&self) -> BTreeSet<ContentKind> {
        self.facets.iter().flat_map(Facet::kinds).collect()
    }

    /// The facets, in preference order.
    #[must_use]
    pub fn facets(&self) -> &[Facet] {
        &self.facets
    }

    /// The representation an action gets when it does not ask for a specific one.
    #[must_use]
    pub fn primary(&self) -> &Facet {
        // Non-empty by construction.
        &self.facets[0]
    }

    /// The text form, if there is one.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.facets.iter().find_map(|f| match f {
            Facet::Text(text) => Some(text.as_str()),
            Facet::Url(url) => Some(url.as_str()),
            _ => None,
        })
    }

    /// The URL, if the content is one.
    #[must_use]
    pub fn url(&self) -> Option<&Url> {
        self.facets.iter().find_map(|f| match f {
            Facet::Url(url) => Some(url.as_ref()),
            _ => None,
        })
    }

    /// Image data held directly on the clipboard.
    #[must_use]
    pub fn image(&self) -> Option<(&str, &[u8])> {
        self.facets.iter().find_map(|f| match f {
            Facet::Image { mime, bytes } => Some((mime.as_str(), bytes.as_slice())),
            _ => None,
        })
    }

    /// The selected files, if any.
    #[must_use]
    pub fn files(&self) -> Option<&[PathBuf]> {
        self.facets.iter().find_map(|f| match f {
            Facet::Files(paths) => Some(paths.as_slice()),
            _ => None,
        })
    }

    /// The rich-text form, if there is one.
    #[must_use]
    pub fn html(&self) -> Option<&str> {
        self.facets.iter().find_map(|f| match f {
            Facet::Html(html) => Some(html.as_str()),
            _ => None,
        })
    }

    /// The single kind that best describes what was copied.
    ///
    /// Used to key a per-type default action. A selection of pictures answers
    /// `Image` rather than `Files`, because "always resize images" is the rule
    /// someone means whether the pictures were pasted or selected in a file
    /// manager.
    #[must_use]
    pub fn source_kind(&self) -> ContentKind {
        match self.primary() {
            Facet::Files(paths) => {
                if crate::files::all_images(paths) {
                    ContentKind::Image
                } else if crate::files::all_videos(paths) {
                    ContentKind::Video
                } else {
                    ContentKind::Files
                }
            }
            Facet::Image { .. } => ContentKind::Image,
            Facet::Html(_) => ContentKind::Html,
            Facet::Url(_) => ContentKind::Url,
            Facet::Text(_) => ContentKind::Text,
        }
    }

    /// The noun for `kind`, as it should read on a button.
    ///
    /// Pluralised from how many items are actually present, so a selection of
    /// three pictures reads "Resize Images" while a single pasted screenshot
    /// reads "Resize Image".
    #[must_use]
    pub fn noun_for(&self, kind: ContentKind) -> &'static str {
        // Only a file selection can hold more than one of anything; image data
        // held directly on the clipboard is always a single image.
        let several = self.image().is_none() && self.files().is_some_and(|f| f.len() > 1);

        match kind {
            ContentKind::Files => {
                if several {
                    "Files"
                } else {
                    "File"
                }
            }
            ContentKind::Video => {
                if several {
                    "Videos"
                } else {
                    "Video"
                }
            }
            ContentKind::Image => {
                if several {
                    "Images"
                } else {
                    "Image"
                }
            }
            ContentKind::Html => "Rich Text",
            ContentKind::Url => "URL",
            ContentKind::Text => "Text",
        }
    }

    /// The bytes to hand to an external command on stdin.
    ///
    /// Image data when the clipboard holds an image, and text otherwise — which
    /// for a file selection means the paths, one per line.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        if let Some((_, bytes)) = self.image() {
            return bytes;
        }
        self.text().map_or(&[], str::as_bytes)
    }

    /// A one-line summary shown at the top of the action dialog, so the user can
    /// confirm what they are about to act on without pasting it somewhere first.
    #[must_use]
    pub fn describe(&self) -> String {
        match self.primary() {
            Facet::Files(paths) => {
                let mut summary = crate::files::describe(paths);
                if self.text().is_some() {
                    summary.push_str(" · also as text");
                }
                summary
            }
            Facet::Image { mime, bytes } => {
                let format = mime.rsplit('/').next().unwrap_or(mime).to_uppercase();
                let size = human_bytes(bytes.len());
                match imagesize::blob_size(bytes) {
                    Ok(dim) => format!("Image · {format} · {}×{} · {size}", dim.width, dim.height),
                    // Dimensions are a nicety; an unreadable header should not
                    // stop the dialog from opening.
                    Err(_) => format!("Image · {format} · {size}"),
                }
            }
            Facet::Url(url) => {
                let host = url.host_str().unwrap_or("unknown host");
                format!(
                    "URL · {host} · {} characters",
                    thousands(url.as_str().chars().count())
                )
            }
            Facet::Html(html) => {
                format!("Rich text · {} characters", thousands(html.chars().count()))
            }
            Facet::Text(text) => {
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
        }
    }
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

/// The same size, as short as it can be written.
///
/// For a button caption, where `512.0 KB` is three characters of nothing:
/// no space before the unit, and no decimal on a whole number.
#[must_use]
pub fn compact_bytes(bytes: usize) -> String {
    #[allow(clippy::cast_precision_loss)] // Display only.
    let value = bytes as f64;
    let (scaled, unit) = if bytes < 1024 {
        return format!("{bytes}B");
    } else if bytes < 1024 * 1024 {
        (value / 1024.0, "KB")
    } else {
        (value / (1024.0 * 1024.0), "MB")
    };

    // A whole number keeps no decimal; 1.5 MB still needs one.
    if (scaled.fract()).abs() < 0.05 {
        format!("{scaled:.0}{unit}")
    } else {
        format!("{scaled:.1}{unit}")
    }
}

/// Reads a size written the way people write sizes: `10mb`, `1.5 GB`, `512k`,
/// or a plain number of bytes.
///
/// The binary meaning is used throughout — `1 KB` is 1024 bytes — because that
/// is what [`human_bytes`] prints, and a value that does not round-trip
/// through its own display is a trap.
#[must_use]
pub fn parse_bytes(text: &str) -> Option<u64> {
    let text = text.trim().to_ascii_lowercase();
    let digits: String = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() {
        return None;
    }

    let value: f64 = digits.parse().ok()?;
    let multiplier = match text[digits.len()..].trim().trim_end_matches('b').trim() {
        "" => 1.0,
        "k" => 1024.0,
        "m" => 1024.0 * 1024.0,
        "g" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };

    let bytes = value * multiplier;
    if !bytes.is_finite() || bytes < 1.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    // Guarded above; anything beyond u64 saturates, which is not a size anyone
    // is asking for.
    Some(bytes as u64)
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
mod size_tests {
    use super::{human_bytes, parse_bytes};

    #[test]
    fn a_size_can_be_written_the_ways_people_write_sizes() {
        assert_eq!(parse_bytes("10mb"), Some(10 * 1024 * 1024));
        assert_eq!(parse_bytes("10 MB"), Some(10 * 1024 * 1024));
        assert_eq!(parse_bytes("10m"), Some(10 * 1024 * 1024));
        assert_eq!(parse_bytes("512k"), Some(512 * 1024));
        assert_eq!(parse_bytes("1.5gb"), Some(1024 * 1024 * 1024 * 3 / 2));
        assert_eq!(parse_bytes("2048"), Some(2048));
    }

    #[test]
    fn a_size_that_is_not_a_size_is_rejected() {
        for bad in ["", "mb", "lots", "10tb", "-5", "0"] {
            assert_eq!(parse_bytes(bad), None, "{bad} should not parse");
        }
    }

    #[test]
    fn a_printed_size_can_be_read_back() {
        // A value that does not survive its own display is a trap.
        for bytes in [512_usize, 10 * 1024, 5 * 1024 * 1024] {
            let printed = human_bytes(bytes);
            assert_eq!(
                parse_bytes(&printed),
                Some(bytes as u64),
                "{printed} did not round-trip"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Clip {
        Clip::from_text(s).expect("non-empty")
    }

    #[test]
    fn a_bare_url_offers_both_url_and_text() {
        let clip = text("https://example.com/a");
        assert!(clip.kinds().contains(&ContentKind::Url));
        // Split and Truncate should still be offered for a URL.
        assert!(clip.kinds().contains(&ContentKind::Text));
        assert_eq!(clip.url().map(Url::as_str), Some("https://example.com/a"));
    }

    #[test]
    fn a_sentence_containing_a_url_is_only_text() {
        let clip = text("see https://example.com for more");
        assert!(!clip.kinds().contains(&ContentKind::Url));
        assert!(clip.kinds().contains(&ContentKind::Text));
    }

    #[test]
    fn non_web_schemes_are_text() {
        for input in [
            "file:///etc/passwd",
            "mailto:a@b.com",
            "javascript:alert(1)",
        ] {
            assert!(
                !text(input).kinds().contains(&ContentKind::Url),
                "{input} should not be read as a URL"
            );
        }
    }

    #[test]
    fn empty_text_is_no_clip_at_all() {
        assert_eq!(Clip::from_text(""), None);
        assert_eq!(Clip::from_text("   \n "), None);
    }

    #[test]
    fn a_file_selection_is_offered_as_files_and_as_text() {
        // Dolphin publishes no text/plain, so the text form is synthesised —
        // without it there would be no way to paste the paths anywhere.
        let clip = Clip::from_files(vec![
            PathBuf::from("/a/one.png"),
            PathBuf::from("/a/two.png"),
        ])
        .expect("non-empty");

        let kinds = clip.kinds();
        assert!(kinds.contains(&ContentKind::Files));
        assert!(kinds.contains(&ContentKind::Text));
        assert!(kinds.contains(&ContentKind::Image), "all-image selection");
        assert_eq!(clip.text(), Some("/a/one.png\n/a/two.png"));
    }

    #[test]
    fn a_mixed_file_selection_is_not_offered_as_images() {
        let clip = Clip::from_files(vec![
            PathBuf::from("/a/one.png"),
            PathBuf::from("/a/notes.txt"),
        ])
        .expect("non-empty");

        assert!(clip.kinds().contains(&ContentKind::Files));
        assert!(!clip.kinds().contains(&ContentKind::Image));
    }

    #[test]
    fn a_video_selection_is_offered_as_video_not_image() {
        let clip = Clip::from_files(vec![PathBuf::from("/a/clip.mp4")]).expect("non-empty");
        assert!(clip.kinds().contains(&ContentKind::Video));
        assert!(!clip.kinds().contains(&ContentKind::Image));
    }

    #[test]
    fn an_empty_file_selection_is_no_clip() {
        assert_eq!(Clip::from_files(Vec::new()), None);
    }

    #[test]
    fn an_image_offers_only_image() {
        let clip = Clip::from_image("image/png".to_string(), vec![1, 2, 3]).expect("non-empty");
        assert_eq!(clip.kinds(), BTreeSet::from([ContentKind::Image]));
        assert!(
            clip.text().is_none(),
            "there is nothing to type for an image"
        );
    }

    #[test]
    fn an_empty_image_is_no_clip() {
        assert_eq!(Clip::from_image("image/png".to_string(), Vec::new()), None);
    }

    #[test]
    fn stdin_bytes_are_the_image_for_an_image_and_text_otherwise() {
        let image = Clip::from_image("image/png".to_string(), vec![9, 9, 9]).expect("non-empty");
        assert_eq!(image.as_bytes(), &[9, 9, 9]);

        let files = Clip::from_files(vec![PathBuf::from("/a/x.png")]).expect("non-empty");
        assert_eq!(files.as_bytes(), b"/a/x.png");

        assert_eq!(text("hello").as_bytes(), b"hello");
    }

    #[test]
    fn the_source_kind_describes_what_was_copied() {
        assert_eq!(text("hello").source_kind(), ContentKind::Text);
        assert_eq!(
            text("https://example.com/a").source_kind(),
            ContentKind::Url
        );
        assert_eq!(
            Clip::from_image("image/png".to_string(), vec![1])
                .expect("non-empty")
                .source_kind(),
            ContentKind::Image
        );
    }

    #[test]
    fn a_picture_selection_reports_images_rather_than_files() {
        // "Always resize images" should apply whether they were pasted or
        // selected in a file manager.
        let clip = Clip::from_files(vec![PathBuf::from("/a/one.png")]).expect("non-empty");
        assert_eq!(clip.source_kind(), ContentKind::Image);

        let videos = Clip::from_files(vec![PathBuf::from("/a/clip.mp4")]).expect("non-empty");
        assert_eq!(videos.source_kind(), ContentKind::Video);

        let mixed = Clip::from_files(vec![
            PathBuf::from("/a/one.png"),
            PathBuf::from("/a/notes.txt"),
        ])
        .expect("non-empty");
        assert_eq!(mixed.source_kind(), ContentKind::Files);
    }

    #[test]
    fn content_kind_round_trips_through_its_config_name() {
        for kind in ContentKind::all() {
            assert_eq!(ContentKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ContentKind::parse("  Image "), Some(ContentKind::Image));
        assert_eq!(ContentKind::parse("file"), Some(ContentKind::Files));
        assert_eq!(ContentKind::parse("audio"), None);
    }

    #[test]
    fn descriptions_name_the_content() {
        assert!(text("https://example.com/x")
            .describe()
            .starts_with("URL · example.com · "));
        assert!(text("hello").describe().contains("5 characters"));
        assert!(text("a\nb").describe().contains("2 lines"));

        let files = Clip::from_files(vec![PathBuf::from("/a/x.png")]).expect("non-empty");
        assert!(
            files.describe().ends_with("· also as text"),
            "{}",
            files.describe()
        );
    }

    #[test]
    fn image_description_falls_back_when_the_header_is_unreadable() {
        let clip = Clip::from_image("image/png".to_string(), vec![0; 10]).expect("non-empty");
        assert!(
            clip.describe().starts_with("Image · PNG · "),
            "{}",
            clip.describe()
        );
    }

    #[test]
    fn pushing_a_duplicate_kind_is_ignored() {
        let mut clip = text("hello");
        clip.push(Facet::Text("other".to_string()));
        assert_eq!(clip.text(), Some("hello"), "the original text must win");
    }

    #[test]
    fn png_is_preferred_over_jpeg() {
        let offered = vec!["image/jpeg".to_string(), "image/png".to_string()];
        assert_eq!(pick_image_mime(&offered).as_deref(), Some("image/png"));
    }

    #[test]
    fn mime_matching_ignores_case_and_accepts_unlisted_image_types() {
        assert_eq!(
            pick_image_mime(&["IMAGE/PNG".to_string()]).as_deref(),
            Some("IMAGE/PNG")
        );
        assert_eq!(
            pick_image_mime(&["text/html".to_string(), "image/tiff".to_string()]).as_deref(),
            Some("image/tiff")
        );
        assert_eq!(pick_image_mime(&["text/plain".to_string()]), None);
    }

    #[test]
    fn thousands_groups_long_numbers() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(1_204), "1 204");
        assert_eq!(thousands(1_000_000), "1 000 000");
    }

    #[test]
    fn compact_bytes_drops_what_a_button_does_not_need() {
        assert_eq!(compact_bytes(900), "900B");
        assert_eq!(compact_bytes(512 * 1024), "512KB");
        assert_eq!(compact_bytes(8 * 1024 * 1024), "8MB");
        // Not every size is whole, and rounding 1.5 to 2 would be a lie.
        assert_eq!(compact_bytes(1536 * 1024), "1.5MB");
        assert_eq!(compact_bytes(1024), "1KB");
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
    }
}

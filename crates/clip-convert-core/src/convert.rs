//! Converting clipboard content from one format to another.
//!
//! The table of what can become what lives in the config file, so a pair the
//! app never heard of is a few lines of TOML rather than a rebuild. An entry
//! with no `command` is done natively, in process; one with a `command` shells
//! out to whatever the user named, with `{input}` and `{output}` standing in
//! for the two file paths.
//!
//! What is *offered* is decided by the formats actually on the clipboard, so
//! the second dialog lists conversions that can really happen rather than the
//! whole table.

use crate::content::Clip;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Largest edge an ICO may have. The format stores the dimension in one byte,
/// with 0 meaning 256, so anything larger simply cannot be written.
const ICO_MAX_EDGE: u32 = 256;

/// One `[[conversions]]` entry as written in the config file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Conversion {
    pub id: String,
    /// Shown on the button. Absent builds one from the target format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Source formats this can start from, as bare extensions.
    pub from: Vec<String>,
    /// Target format, as a bare extension. Also the result's extension.
    pub to: String,
    /// The program and arguments to run, with `{input}` and `{output}`
    /// replaced by file paths. Empty means the conversion is done natively.
    ///
    /// An argument vector, never a shell string: clipboard content is
    /// untrusted input and must not reach a shell.
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default = "crate::config::default_true")]
    pub enabled: bool,
    /// A small picture shown on this conversion's button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Colour of this conversion's button, as a hex string such as `#3b5bdb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button_color: Option<String>,
    /// Colour of the text on this conversion's button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
}

impl Conversion {
    /// Whether this is done in process rather than by an external program.
    #[must_use]
    pub fn is_native(&self) -> bool {
        self.command.is_empty()
    }

    /// The button text: what was configured, or one built from the target.
    #[must_use]
    pub fn display_label(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| format!("To {}", self.to.to_uppercase()))
    }

    /// Whether this conversion can start from any of `sources`.
    ///
    /// A conversion to the format the content already is would do nothing, so
    /// it is not offered.
    #[must_use]
    pub fn applies_to(&self, sources: &BTreeSet<String>) -> bool {
        if sources.len() == 1 && sources.contains(&normalise(&self.to)) {
            return false;
        }
        self.from
            .iter()
            .any(|from| sources.contains(&normalise(from)))
    }
}

/// Lower-cases a format name and folds the spellings that mean one thing.
#[must_use]
pub fn normalise(format: &str) -> String {
    match format
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "jpeg".to_string(),
        "markdown" => "md".to_string(),
        "text" => "txt".to_string(),
        "htm" => "html".to_string(),
        other => other.to_string(),
    }
}

/// The formats the clipboard currently offers, as bare extensions.
#[must_use]
pub fn source_formats(clip: &Clip) -> BTreeSet<String> {
    let mut formats = BTreeSet::new();

    if let Some(paths) = clip.files() {
        for path in paths {
            if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
                formats.insert(normalise(extension));
            }
        }
    }
    if let Some((mime, bytes)) = clip.image() {
        // The bytes are authoritative; the advertised MIME type is a hint that
        // a program on the other end of the clipboard may well have got wrong.
        let guessed = image::guess_format(bytes)
            .ok()
            .and_then(|format| format.extensions_str().first().copied().map(str::to_string));
        formats.insert(guessed.unwrap_or_else(|| normalise(mime.rsplit('/').next().unwrap_or(""))));
    }
    if clip.html().is_some() {
        formats.insert("html".to_string());
    }

    formats.remove("");
    formats
}

/// The conversions worth offering for content in `sources`, in config order.
#[must_use]
pub fn applicable<'a>(list: &'a [Conversion], sources: &BTreeSet<String>) -> Vec<&'a Conversion> {
    list.iter()
        .filter(|entry| entry.enabled && entry.applies_to(sources))
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("`{format}` is not an image format this can write")]
    UnknownTarget { format: String },
    #[error("could not read the image: {0}")]
    Decode(#[source] image::ImageError),
    #[error("could not write the image: {0}")]
    Encode(#[source] image::ImageError),
    #[error(transparent)]
    Webp(#[from] crate::encode::EncodeError),
}

/// Re-encodes an image to `to`, in process.
///
/// # Errors
///
/// Returns [`ConvertError::UnknownTarget`] for a format this cannot write, or
/// the decode or encode failure from the image itself.
pub fn image_to(bytes: &[u8], to: &str) -> Result<Vec<u8>, ConvertError> {
    let target = normalise(to);

    // WebP is the one format `image` reads but does not write, so it goes
    // through the same encoder the resize built-in uses.
    if target == "webp" {
        let decoded = image::load_from_memory(bytes).map_err(ConvertError::Decode)?;
        return Ok(crate::encode::encode(
            &decoded,
            crate::encode::Format::Webp,
            crate::encode::TRUECOLOR_ABOVE,
        )?);
    }

    let format = image::ImageFormat::from_extension(&target)
        .filter(image::ImageFormat::writing_enabled)
        .ok_or_else(|| ConvertError::UnknownTarget {
            format: target.clone(),
        })?;

    let mut decoded = image::load_from_memory(bytes).map_err(ConvertError::Decode)?;
    if format == image::ImageFormat::Ico {
        decoded = fit_for_ico(decoded);
    }

    let mut out = std::io::Cursor::new(Vec::new());
    decoded
        .write_to(&mut out, format)
        .map_err(ConvertError::Encode)?;
    Ok(out.into_inner())
}

/// Shrinks an image to what an ICO can actually hold, if it is larger.
fn fit_for_ico(image: image::DynamicImage) -> image::DynamicImage {
    let (width, height) = (image.width(), image.height());
    if width <= ICO_MAX_EDGE && height <= ICO_MAX_EDGE {
        return image;
    }
    let (width, height) = crate::presets::fit_inside(width, height, ICO_MAX_EDGE, ICO_MAX_EDGE);
    image.resize_exact(width, height, image::imageops::FilterType::Lanczos3)
}

/// Turns HTML into Markdown.
#[must_use]
pub fn html_to_markdown(html: &str) -> String {
    html2md::parse_html(html).trim().to_string()
}

/// The conversions written into a fresh config file.
///
/// The image entries are one per target rather than one per pair: every image
/// format here can be read, so listing pairs would be a table of forty-odd
/// rows that says the same thing.
#[must_use]
pub fn factory() -> Vec<Conversion> {
    const IMAGES: [&str; 7] = ["png", "jpeg", "webp", "gif", "bmp", "tiff", "ico"];

    let mut list: Vec<Conversion> = IMAGES
        .iter()
        .map(|to| Conversion {
            id: format!("to-{to}"),
            label: None,
            from: IMAGES.iter().map(|f| (*f).to_string()).collect(),
            to: (*to).to_string(),
            command: Vec::new(),
            enabled: true,
            icon: None,
            button_color: None,
            text_color: None,
        })
        .collect();

    list.push(Conversion {
        id: "html-to-markdown".to_string(),
        label: Some("To Markdown".to_string()),
        from: vec!["html".to_string()],
        to: "md".to_string(),
        command: Vec::new(),
        enabled: true,
        icon: None,
        button_color: None,
        text_color: None,
    });
    // The one factory entry that needs something installed: poppler's
    // `pdftotext`. It is disabled rather than absent, so it is discoverable in
    // the config file by anyone who has it.
    list.push(Conversion {
        id: "pdf-to-text".to_string(),
        label: Some("To text".to_string()),
        from: vec!["pdf".to_string()],
        to: "txt".to_string(),
        command: vec![
            "pdftotext".to_string(),
            "{input}".to_string(),
            "{output}".to_string(),
        ],
        enabled: false,
        icon: None,
        button_color: None,
        text_color: None,
    });
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sources(of: &[&str]) -> BTreeSet<String> {
        of.iter().map(|s| normalise(s)).collect()
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::DynamicImage::new_rgba8(width, height);
        let mut out = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encodes");
        out.into_inner()
    }

    #[test]
    fn spellings_of_the_same_format_fold_together() {
        assert_eq!(normalise("JPG"), normalise("jpeg"));
        assert_eq!(normalise(".PNG"), "png");
        assert_eq!(normalise("Markdown"), "md");
    }

    #[test]
    fn a_conversion_to_what_it_already_is_is_not_offered() {
        let to_png = factory()
            .into_iter()
            .find(|c| c.id == "to-png")
            .expect("to-png");
        assert!(!to_png.applies_to(&sources(&["png"])));
        assert!(to_png.applies_to(&sources(&["jpg"])));
        // A mixed selection can still usefully be made uniform.
        assert!(to_png.applies_to(&sources(&["png", "jpg"])));
    }

    #[test]
    fn only_conversions_the_clipboard_can_feed_are_offered() {
        let list = factory();
        let offered: Vec<&str> = applicable(&list, &sources(&["html"]))
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(offered, ["html-to-markdown"]);
    }

    #[test]
    fn a_disabled_conversion_is_not_offered() {
        let list = factory();
        let offered = applicable(&list, &sources(&["pdf"]));
        assert!(
            offered.is_empty(),
            "the pdf entry ships disabled: {offered:?}"
        );
    }

    #[test]
    fn the_formats_of_a_file_selection_are_its_extensions() {
        let clip = Clip::from_files(vec![
            PathBuf::from("/a/one.PNG"),
            PathBuf::from("/a/two.jpg"),
        ])
        .expect("non-empty");
        assert_eq!(source_formats(&clip), sources(&["png", "jpeg"]));
    }

    #[test]
    fn the_format_of_pasted_image_data_comes_from_its_bytes() {
        // Deliberately mislabelled: a program on the other end of the clipboard
        // may advertise anything, but the bytes cannot lie.
        let clip = Clip::from_image("image/jpeg".to_string(), png(4, 4)).expect("non-empty");
        assert_eq!(source_formats(&clip), sources(&["png"]));
    }

    #[test]
    fn every_factory_image_target_can_actually_be_written() {
        let source = png(8, 8);
        for conversion in factory().iter().filter(|c| c.is_native() && c.to != "md") {
            let out = image_to(&source, &conversion.to)
                .unwrap_or_else(|e| panic!("{}: {e}", conversion.id));
            assert!(!out.is_empty(), "{} produced nothing", conversion.id);
            // The result must be readable as the format it claims to be.
            let guessed = image::guess_format(&out).expect("a known format");
            assert!(
                guessed
                    .extensions_str()
                    .iter()
                    .any(|e| normalise(e) == normalise(&conversion.to)),
                "{} produced {guessed:?}",
                conversion.id
            );
        }
    }

    #[test]
    fn an_image_too_large_for_an_ico_is_shrunk_rather_than_refused() {
        let out = image_to(&png(1024, 512), "ico").expect("an ICO");
        let decoded = image::load_from_memory(&out).expect("readable");
        assert!(decoded.width() <= ICO_MAX_EDGE && decoded.height() <= ICO_MAX_EDGE);
        // The aspect ratio is kept, so the icon is not squashed.
        assert_eq!(decoded.width(), 256);
        assert_eq!(decoded.height(), 128);
    }

    #[test]
    fn an_unwritable_target_is_named_rather_than_silently_skipped() {
        match image_to(&png(4, 4), "psd") {
            Err(ConvertError::UnknownTarget { format }) => assert_eq!(format, "psd"),
            other => panic!("expected UnknownTarget, got {other:?}"),
        }
    }

    #[test]
    fn html_becomes_markdown() {
        let markdown = html_to_markdown("<h1>Title</h1><p>Some <b>bold</b> text.</p>");
        // A setext heading — underlined rather than prefixed — is what this
        // writer produces, and is valid Markdown.
        assert!(markdown.starts_with("Title\n=="), "{markdown}");
        assert!(markdown.contains("**bold**"), "{markdown}");
        assert!(
            !markdown.contains('<'),
            "no markup should survive: {markdown}"
        );
    }

    #[test]
    fn a_label_is_built_from_the_target_when_none_is_configured() {
        let to_ico = factory()
            .into_iter()
            .find(|c| c.id == "to-ico")
            .expect("to-ico");
        assert_eq!(to_ico.display_label(), "To ICO");
    }

    #[test]
    fn factory_conversions_are_uniquely_identified() {
        let list = factory();
        let ids: BTreeSet<&str> = list.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.len(), list.len());
    }
}

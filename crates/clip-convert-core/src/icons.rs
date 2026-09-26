//! Icons for actions and size targets.
//!
//! An icon may be written in the config as a `data:` URI, bare base64, SVG
//! markup written out in full, an `http(s)` URL, or a path to a local file. Anything that is not already
//! base64 is fetched or read once and **written back to the config as base64**,
//! so the app never depends on that URL or file again — a config file that
//! still works after the source disappears, and no network access on the path
//! that opens a menu.

use base64::Engine as _;
use std::time::Duration;

/// How long to wait for an icon to download.
///
/// Short: this happens at startup, and a missing icon is not worth delaying a
/// tray app for.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest icon accepted, encoded.
///
/// Icons live in a config file and travel to the dialog process on every
/// prompt, so a megabyte-sized one would be paid for repeatedly.
const MAX_BYTES: usize = 256 * 1024;

/// The engine used for every encode and decode here.
const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// What an icon field in the config turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    /// Already base64, possibly behind a `data:` prefix.
    Encoded(String),
    /// Something to download.
    Url(String),
    /// Something to read from disk.
    Path(std::path::PathBuf),
    /// SVG written out in the config file itself.
    Markup(String),
}

/// Works out how an icon field should be treated.
#[must_use]
pub fn classify(raw: &str) -> Option<Source> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(payload) = trimmed.strip_prefix("data:") {
        // `data:image/png;base64,AAAA…`
        let encoded = payload.split_once(',').map_or(payload, |(_, rest)| rest);
        return Some(Source::Encoded(encoded.trim().to_string()));
    }

    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Some(Source::Url(trimmed.to_string()));
    }

    // SVG written out in full. Checked before base64, which it could never be
    // mistaken for, and before paths, which it would otherwise become.
    if is_svg(trimmed.as_bytes()) {
        return Some(Source::Markup(trimmed.to_string()));
    }

    // Base64 before paths, decided by what the bytes actually are rather than
    // by punctuation: `/` and `+` are both in the base64 alphabet, so a test
    // for a path separator classifies real base64 as a file path.
    if let Ok(bytes) = BASE64.decode(trimmed) {
        if image::guess_format(&bytes).is_ok() {
            return Some(Source::Encoded(trimmed.to_string()));
        }
    }

    Some(Source::Path(std::path::PathBuf::from(trimmed)))
}

#[derive(Debug, thiserror::Error)]
pub enum IconError {
    #[error("could not read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not download {url}: {source}")]
    Download {
        url: String,
        #[source]
        source: Box<ureq::Error>,
    },
    #[error("the icon is {size} bytes, over the {MAX_BYTES} byte limit")]
    TooBig { size: usize },
    #[error("the icon is not a readable image: {0}")]
    NotAnImage(#[source] image::ImageError),
    #[error("the icon is not readable SVG: {0}")]
    BadSvg(#[source] Box<resvg::usvg::Error>),
    #[error("the SVG has no drawable size")]
    EmptySvg,
    #[error("the rasterised icon could not be encoded: {0}")]
    Render(String),
}

/// Longest edge a stored icon may have.
///
/// Buttons draw an icon at a fraction of this, so anything larger is weight in
/// the config file and work on every frame for detail nobody sees. Vector art
/// is rasterised at this size and a large bitmap is scaled down to it.
const MAX_EDGE: u32 = 256;

/// Turns an icon field into base64, fetching or reading it if needed.
///
/// Returns `Ok(None)` when the field was already base64 and needs no rewriting.
///
/// # Errors
///
/// Returns an error if the source could not be read, was too large, or was not
/// a readable image.
pub fn resolve(raw: &str, ignore_ssl: bool) -> Result<Option<String>, IconError> {
    let bytes = match classify(raw) {
        None | Some(Source::Encoded(_)) => return Ok(None),
        Some(Source::Path(path)) => std::fs::read(&path).map_err(|source| IconError::Read {
            path: path.clone(),
            source,
        })?,
        Some(Source::Url(url)) => download(&url, ignore_ssl)?,
        Some(Source::Markup(svg)) => svg.into_bytes(),
    };

    if bytes.len() > MAX_BYTES {
        return Err(IconError::TooBig { size: bytes.len() });
    }

    // The dialog draws bitmaps, so vector art is rasterised here — once, at
    // load, rather than on every prompt.
    let bytes = if is_svg(&bytes) {
        rasterise_svg(&bytes)?
    } else {
        bytes
    };

    // Decoded now rather than when the menu opens, so a broken icon is
    // reported at startup instead of producing an empty button later.
    let decoded = image::load_from_memory(&bytes).map_err(IconError::NotAnImage)?;
    let bytes = shrink_to_fit(&decoded, bytes)?;

    Ok(Some(BASE64.encode(&bytes)))
}

/// Scales an oversized icon down to [`MAX_EDGE`], leaving smaller ones alone.
///
/// Returns PNG, because a scaled image has to be re-encoded and PNG is the one
/// format that keeps the transparency these icons rely on.
fn shrink_to_fit(decoded: &image::DynamicImage, original: Vec<u8>) -> Result<Vec<u8>, IconError> {
    let (width, height) = (decoded.width(), decoded.height());
    if width <= MAX_EDGE && height <= MAX_EDGE {
        return Ok(original);
    }

    let (width, height) = crate::presets::fit_inside(width, height, MAX_EDGE, MAX_EDGE);
    let scaled = decoded.resize_exact(width, height, image::imageops::FilterType::Lanczos3);

    crate::encode::encode(
        &scaled,
        crate::encode::Format::Png,
        crate::encode::TRUECOLOR_ABOVE,
    )
    .map_err(|e| IconError::Render(e.to_string()))
}

/// Whether these bytes are an SVG document.
fn is_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start();
    text.starts_with("<svg") || (text.starts_with("<?xml") && text.contains("<svg"))
}

/// Renders an SVG to a PNG of [`MAX_EDGE`], fitted and centred.
///
/// A monochrome icon that names no colour of its own is drawn white. These
/// dialogs are dark, and the usual source of such an icon — a brand mark from
/// an icon set — defaults to black, which would be invisible.
fn rasterise_svg(bytes: &[u8]) -> Result<Vec<u8>, IconError> {
    let source = String::from_utf8_lossy(bytes);
    let source = if source.contains("fill=") {
        source.into_owned()
    } else {
        source.replacen("<svg", r##"<svg fill="#ffffff""##, 1)
    };

    let tree = resvg::usvg::Tree::from_str(&source, &resvg::usvg::Options::default())
        .map_err(|e| IconError::BadSvg(Box::new(e)))?;

    let size = tree.size();
    let (width, height) = (size.width(), size.height());
    if width <= 0.0 || height <= 0.0 {
        return Err(IconError::EmptySvg);
    }

    #[allow(clippy::cast_precision_loss)] // MAX_EDGE is 64.
    let scale = (MAX_EDGE as f32 / width).min(MAX_EDGE as f32 / height);
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(MAX_EDGE, MAX_EDGE).ok_or(IconError::EmptySvg)?;

    // Centred in the square, so icons of different aspect ratios line up with
    // each other on their buttons.
    #[allow(clippy::cast_precision_loss)]
    let transform = resvg::tiny_skia::Transform::from_translate(
        (MAX_EDGE as f32 - width * scale) / 2.0,
        (MAX_EDGE as f32 - height * scale) / 2.0,
    )
    .pre_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    pixmap
        .encode_png()
        .map_err(|e| IconError::Render(e.to_string()))
}

fn download(url: &str, ignore_ssl: bool) -> Result<Vec<u8>, IconError> {
    use std::io::Read as _;

    let fail = |source: ureq::Error| IconError::Download {
        url: url.to_string(),
        source: Box::new(source),
    };

    // Through the shared agent: a plain `ureq::get` has no TLS backend in this
    // build and fails on every https:// address.
    let response = crate::http::agent(ignore_ssl, FETCH_TIMEOUT)
        .get(url)
        .call()
        .map_err(fail)?;

    let mut bytes = Vec::new();
    // Bounded read: a server answering with something enormous must not be
    // able to exhaust memory.
    response
        .into_reader()
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| IconError::Download {
            url: url.to_string(),
            source: Box::new(ureq::Error::from(e)),
        })?;
    Ok(bytes)
}

/// Decodes an icon field into image bytes, ready to be drawn.
#[must_use]
pub fn decode(raw: &str) -> Option<Vec<u8>> {
    match classify(raw)? {
        Source::Encoded(encoded) => BASE64.decode(encoded).ok(),
        // Anything unresolved is skipped rather than fetched or rendered here:
        // this runs in the dialog, which must not touch the network to draw a
        // button.
        Source::Url(_) | Source::Path(_) | Source::Markup(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest valid PNG, as bytes.
    fn tiny_png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
        crate::encode::encode(
            &image::DynamicImage::ImageRgba8(img),
            crate::encode::Format::Png,
            95,
        )
        .expect("encodes")
    }

    #[test]
    fn svg_written_out_in_the_config_is_rendered_like_any_other() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/></svg>"#;
        assert_eq!(classify(svg), Some(Source::Markup(svg.to_string())));

        let encoded = resolve(svg, false)
            .expect("renders")
            .expect("rewritten as base64");
        let stored = image::load_from_memory(&BASE64.decode(encoded).expect("base64"))
            .expect("a readable image");
        assert_eq!((stored.width(), stored.height()), (MAX_EDGE, MAX_EDGE));

        // Drawn white on transparent, like the ones fetched from a URL.
        let rgba = stored.to_rgba8();
        assert_eq!(
            rgba.get_pixel(MAX_EDGE / 2, MAX_EDGE / 2).0,
            [255, 255, 255, 255]
        );
        assert_eq!(rgba.get_pixel(0, 0).0[3], 0);
    }

    #[test]
    fn an_svg_is_rasterised_on_the_way_in() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect width="24" height="12" /></svg>"#;
        assert!(is_svg(svg));

        let png = rasterise_svg(svg).expect("renders");
        let image = image::load_from_memory(&png).expect("a readable image");
        assert_eq!((image.width(), image.height()), (MAX_EDGE, MAX_EDGE));

        // Drawn white, because the source named no colour and these dialogs
        // are dark. The rect covers the top half, centred vertically.
        let rgba = image.to_rgba8();
        let inside = rgba.get_pixel(MAX_EDGE / 2, MAX_EDGE / 2 - 8);
        assert_eq!(inside.0, [255, 255, 255, 255], "{inside:?}");
        // And the space around it stays transparent.
        assert_eq!(rgba.get_pixel(0, MAX_EDGE - 1).0[3], 0);
    }

    #[test]
    fn an_oversized_icon_is_scaled_down_to_the_limit() {
        let big = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1000,
            500,
            image::Rgba([1, 2, 3, 255]),
        ));
        let png = crate::encode::encode(&big, crate::encode::Format::Png, 95).expect("encodes");

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.png");
        std::fs::write(&path, png).expect("write");

        let encoded = resolve(path.to_str().expect("utf-8"), false)
            .expect("resolves")
            .expect("rewritten");
        let stored =
            image::load_from_memory(&BASE64.decode(encoded).expect("base64")).expect("readable");

        assert_eq!((stored.width(), stored.height()), (MAX_EDGE, MAX_EDGE / 2));
    }

    #[test]
    fn an_icon_already_small_enough_is_stored_untouched() {
        let png = tiny_png();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("small.png");
        std::fs::write(&path, &png).expect("write");

        let encoded = resolve(path.to_str().expect("utf-8"), false)
            .expect("resolves")
            .expect("rewritten");
        assert_eq!(
            BASE64.decode(encoded).expect("base64"),
            png,
            "re-encoding a small icon is pointless work"
        );
    }

    #[test]
    fn an_svg_that_names_its_own_colour_keeps_it() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect width="24" height="24" fill="#ff0000" /></svg>"##;
        let png = rasterise_svg(svg).expect("renders");
        let rgba = image::load_from_memory(&png).expect("readable").to_rgba8();
        assert_eq!(
            rgba.get_pixel(MAX_EDGE / 2, MAX_EDGE / 2).0,
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn something_that_is_not_svg_is_not_treated_as_svg() {
        assert!(!is_svg(&tiny_png()));
        assert!(!is_svg(b"<html><body>no</body></html>"));
    }

    #[test]
    fn a_data_uri_is_recognised_and_stripped() {
        let source = classify("data:image/png;base64,AAAA").expect("classified");
        assert_eq!(source, Source::Encoded("AAAA".to_string()));
    }

    #[test]
    fn bare_base64_is_left_alone() {
        let encoded = BASE64.encode(tiny_png());
        assert_eq!(classify(&encoded), Some(Source::Encoded(encoded.clone())));
        assert_eq!(resolve(&encoded, false).expect("no work to do"), None);
        assert_eq!(decode(&encoded), Some(tiny_png()));
    }

    #[test]
    fn base64_containing_a_slash_is_not_mistaken_for_a_path() {
        // Regression: `/` and `+` are in the base64 alphabet, so classifying on
        // punctuation treated real icon data as a file path and the icon never
        // rendered. Decided by the decoded bytes instead.
        let mut encoded = String::new();
        let mut attempts = 0;
        while !encoded.contains('/') && attempts < 64 {
            let noise: Vec<u8> = (0..=255u8).cycle().take(512 + attempts).collect();
            let img = image::RgbaImage::from_raw(8, 16, noise[..512].to_vec()).expect("8x16 rgba");
            let png = crate::encode::encode(
                &image::DynamicImage::ImageRgba8(img),
                crate::encode::Format::Png,
                40 + u8::try_from(attempts).unwrap_or(0),
            )
            .expect("encodes");
            encoded = BASE64.encode(&png);
            attempts += 1;
        }
        assert!(encoded.contains('/'), "needed base64 containing a slash");

        assert_eq!(
            classify(&encoded),
            Some(Source::Encoded(encoded.clone())),
            "base64 with a slash must not be read as a path"
        );
        assert!(decode(&encoded).is_some());
    }

    #[test]
    fn a_path_without_a_separator_is_still_a_path() {
        assert_eq!(classify("icon.png"), Some(Source::Path("icon.png".into())));
    }

    #[test]
    fn urls_and_paths_are_told_apart() {
        assert_eq!(
            classify("https://example.com/i.png"),
            Some(Source::Url("https://example.com/i.png".to_string()))
        );
        assert_eq!(
            classify("/home/user/i.png"),
            Some(Source::Path("/home/user/i.png".into()))
        );
        assert_eq!(
            classify("./icons/i.png"),
            Some(Source::Path("./icons/i.png".into()))
        );
    }

    #[test]
    fn an_empty_field_is_no_icon() {
        assert_eq!(classify(""), None);
        assert_eq!(classify("   "), None);
        assert_eq!(decode(""), None);
    }

    #[test]
    fn a_local_file_is_read_and_encoded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("icon.png");
        std::fs::write(&path, tiny_png()).expect("write");

        let encoded = resolve(path.to_str().expect("utf-8"), false)
            .expect("resolves")
            .expect("was rewritten");

        // The result must be usable by the dialog without further work.
        let decoded = decode(&encoded).expect("decodes");
        assert_eq!(decoded, tiny_png());
        assert!(image::load_from_memory(&decoded).is_ok());
    }

    #[test]
    fn a_missing_file_is_reported_rather_than_silently_dropped() {
        let err = resolve("/definitely/not/here.png", false).expect_err("missing");
        assert!(matches!(err, IconError::Read { .. }), "{err:?}");
    }

    #[test]
    fn something_that_is_not_an_image_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("icon.png");
        std::fs::write(&path, b"this is not a png").expect("write");

        let err = resolve(path.to_str().expect("utf-8"), false).expect_err("not an image");
        assert!(matches!(err, IconError::NotAnImage(_)), "{err:?}");
    }

    #[test]
    fn an_oversized_icon_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("huge.png");
        std::fs::write(&path, vec![0u8; MAX_BYTES + 1]).expect("write");

        let err = resolve(path.to_str().expect("utf-8"), false).expect_err("too big");
        assert!(matches!(err, IconError::TooBig { .. }), "{err:?}");
    }

    #[test]
    fn an_unresolved_source_does_not_decode() {
        // The dialog must never fetch anything to draw a button.
        assert_eq!(decode("https://example.com/i.png"), None);
        assert_eq!(decode("/home/user/i.png"), None);
    }

    #[test]
    fn encoding_round_trips() {
        let original = tiny_png();
        let encoded = BASE64.encode(&original);
        assert_eq!(decode(&encoded), Some(original));
    }
}

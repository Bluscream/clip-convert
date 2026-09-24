//! Icons for actions and size targets.
//!
//! An icon may be written in the config as a `data:` URI, bare base64, an
//! `http(s)` URL, or a path to a local file. Anything that is not already
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
}

/// Turns an icon field into base64, fetching or reading it if needed.
///
/// Returns `Ok(None)` when the field was already base64 and needs no rewriting.
///
/// # Errors
///
/// Returns an error if the source could not be read, was too large, or was not
/// a readable image.
pub fn resolve(raw: &str) -> Result<Option<String>, IconError> {
    let bytes = match classify(raw) {
        None | Some(Source::Encoded(_)) => return Ok(None),
        Some(Source::Path(path)) => std::fs::read(&path).map_err(|source| IconError::Read {
            path: path.clone(),
            source,
        })?,
        Some(Source::Url(url)) => download(&url)?,
    };

    if bytes.len() > MAX_BYTES {
        return Err(IconError::TooBig { size: bytes.len() });
    }
    // Checked now rather than when the menu opens, so a broken icon is reported
    // at startup instead of producing an empty button later.
    image::load_from_memory(&bytes).map_err(IconError::NotAnImage)?;

    Ok(Some(BASE64.encode(&bytes)))
}

fn download(url: &str) -> Result<Vec<u8>, IconError> {
    use std::io::Read as _;

    let fail = |source: ureq::Error| IconError::Download {
        url: url.to_string(),
        source: Box::new(source),
    };

    let response = ureq::get(url).timeout(FETCH_TIMEOUT).call().map_err(fail)?;

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
        // Anything unresolved is skipped rather than fetched here: this runs in
        // the dialog, which must not touch the network to draw a button.
        Source::Url(_) | Source::Path(_) => None,
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
    fn a_data_uri_is_recognised_and_stripped() {
        let source = classify("data:image/png;base64,AAAA").expect("classified");
        assert_eq!(source, Source::Encoded("AAAA".to_string()));
    }

    #[test]
    fn bare_base64_is_left_alone() {
        let encoded = BASE64.encode(tiny_png());
        assert_eq!(classify(&encoded), Some(Source::Encoded(encoded.clone())));
        assert_eq!(resolve(&encoded).expect("no work to do"), None);
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
            classify("/home/blu/i.png"),
            Some(Source::Path("/home/blu/i.png".into()))
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

        let encoded = resolve(path.to_str().expect("utf-8"))
            .expect("resolves")
            .expect("was rewritten");

        // The result must be usable by the dialog without further work.
        let decoded = decode(&encoded).expect("decodes");
        assert_eq!(decoded, tiny_png());
        assert!(image::load_from_memory(&decoded).is_ok());
    }

    #[test]
    fn a_missing_file_is_reported_rather_than_silently_dropped() {
        let err = resolve("/definitely/not/here.png").expect_err("missing");
        assert!(matches!(err, IconError::Read { .. }), "{err:?}");
    }

    #[test]
    fn something_that_is_not_an_image_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("icon.png");
        std::fs::write(&path, b"this is not a png").expect("write");

        let err = resolve(path.to_str().expect("utf-8")).expect_err("not an image");
        assert!(matches!(err, IconError::NotAnImage(_)), "{err:?}");
    }

    #[test]
    fn an_oversized_icon_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("huge.png");
        std::fs::write(&path, vec![0u8; MAX_BYTES + 1]).expect("write");

        let err = resolve(path.to_str().expect("utf-8")).expect_err("too big");
        assert!(matches!(err, IconError::TooBig { .. }), "{err:?}");
    }

    #[test]
    fn an_unresolved_source_does_not_decode() {
        // The dialog must never fetch anything to draw a button.
        assert_eq!(decode("https://example.com/i.png"), None);
        assert_eq!(decode("/home/blu/i.png"), None);
    }

    #[test]
    fn encoding_round_trips() {
        let original = tiny_png();
        let encoded = BASE64.encode(&original);
        assert_eq!(decode(&encoded), Some(original));
    }
}

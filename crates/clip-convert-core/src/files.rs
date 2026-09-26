//! File lists on the clipboard.
//!
//! When a file manager copies files it does not put paths on the clipboard as
//! plain text. It offers `text/uri-list`: one `file://` URI per line, CRLF
//! separated, percent-encoded, with `#` comment lines. Most managers *also*
//! offer `text/plain` containing the bare paths, which is what makes it possible
//! to treat the same selection either as files or as text.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Extensions treated as still images.
const IMAGE_EXTENSIONS: [&str; 10] = [
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "avif", "ico",
];

/// Extensions treated as video.
const VIDEO_EXTENSIONS: [&str; 9] = [
    "mp4", "m4v", "mkv", "webm", "mov", "avi", "wmv", "flv", "mpg",
];

/// Parses a `text/uri-list` body into local paths.
///
/// Non-local URIs (`https://…`) and comment lines are skipped: an action that
/// works on files has nothing to do with a remote URI, and silently turning one
/// into a path would be wrong.
#[must_use]
pub fn parse_uri_list(body: &str) -> Vec<PathBuf> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| url::Url::parse(line).ok())
        .filter(|url| url.scheme() == "file")
        // `to_file_path` handles the percent-decoding, so a path with spaces or
        // non-ASCII characters survives intact.
        .filter_map(|url| url.to_file_path().ok())
        .collect()
}

/// Renders paths back into a `text/uri-list` body.
///
/// Used when an action produces files and puts them back on the clipboard.
#[must_use]
pub fn to_uri_list(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for path in paths {
        if let Ok(url) = url::Url::from_file_path(path) {
            out.push_str(url.as_str());
            out.push_str("\r\n");
        }
    }
    out
}

/// The lowercase extension of `path`, if it has one.
fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Whether every path looks like a still image.
///
/// Judged by extension rather than by reading the files: the menu has to be
/// built immediately on a hotkey press, and opening a hundred files to sniff
/// their headers would be felt.
#[must_use]
pub fn all_images(paths: &[PathBuf]) -> bool {
    !paths.is_empty()
        && paths
            .iter()
            .all(|p| extension(p).is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str())))
}

/// Whether every path looks like a video.
#[must_use]
pub fn all_videos(paths: &[PathBuf]) -> bool {
    !paths.is_empty()
        && paths
            .iter()
            .all(|p| extension(p).is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.as_str())))
}

/// A short summary of a file list for the dialog header.
#[must_use]
pub fn describe(paths: &[PathBuf]) -> String {
    let count = paths.len();
    let noun = if count == 1 { "file" } else { "files" };

    // Sizes come from metadata, which may be unreadable for a path that has
    // since been moved; an unknown size should not stop the menu appearing.
    let total: u64 = paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();

    let kind = if all_images(paths) {
        " · images"
    } else if all_videos(paths) {
        " · video"
    } else {
        ""
    };

    if total == 0 {
        format!("{count} {noun}{kind}")
    } else {
        format!(
            "{count} {noun}{kind} · {}",
            crate::content::human_bytes(usize::try_from(total).unwrap_or(usize::MAX))
        )
    }
}

/// The distinct extensions in a list, lowercase, for the Convert dialog.
#[must_use]
pub fn extensions(paths: &[PathBuf]) -> BTreeSet<String> {
    paths.iter().filter_map(|p| extension(p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_uri_becomes_a_path() {
        let paths = parse_uri_list("file:///home/user/a.png");
        assert_eq!(paths, [PathBuf::from("/home/user/a.png")]);
    }

    #[test]
    fn a_crlf_list_from_a_file_manager_parses() {
        // This is the shape a file manager actually sends.
        let body = "file:///home/user/one.png\r\nfile:///home/user/two.png\r\n";
        assert_eq!(parse_uri_list(body).len(), 2);
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let body = "# some comment\r\n\r\nfile:///tmp/a.txt\r\n";
        assert_eq!(parse_uri_list(body), [PathBuf::from("/tmp/a.txt")]);
    }

    #[test]
    fn percent_encoding_is_decoded() {
        // A path with spaces and non-ASCII must survive intact, which is the
        // whole reason uri-list is percent-encoded.
        let paths = parse_uri_list("file:///home/user/My%20Photos/caf%C3%A9.png");
        assert_eq!(paths, [PathBuf::from("/home/user/My Photos/café.png")]);
    }

    #[test]
    fn remote_uris_are_not_turned_into_paths() {
        let body = "https://example.com/a.png\r\nfile:///tmp/b.png\r\n";
        assert_eq!(parse_uri_list(body), [PathBuf::from("/tmp/b.png")]);
    }

    #[test]
    fn rubbish_lines_are_ignored_rather_than_guessed_at() {
        assert!(parse_uri_list("not a uri at all").is_empty());
        assert!(parse_uri_list("").is_empty());
    }

    #[test]
    fn paths_round_trip_through_a_uri_list() {
        let original = vec![
            PathBuf::from("/tmp/one two.png"),
            PathBuf::from("/tmp/café.jpg"),
        ];
        let body = to_uri_list(&original);
        assert!(body.ends_with("\r\n"), "uri-list lines are CRLF terminated");
        assert_eq!(parse_uri_list(&body), original);
    }

    /// Captured from a real Dolphin file copy on KDE/Wayland.
    ///
    /// Note the doubled space encoded as `%20%20`, the `@` in a filename, and
    /// the CRLF line endings — all of which a hand-rolled splitter gets wrong.
    const DOLPHIN_SAMPLE: &str = concat!(
        "file:///mnt/data/Pictures/netspeedmonitor%20%20majorgeeks1.jpg\r\n",
        "file:///mnt/data/Pictures/bliss_4k.png\r\n",
        "file:///mnt/data/Pictures/Snapshot@2022_0311_173322.jpg\r\n",
        "file:///mnt/data/Pictures/menu-buy.gif\r\n",
    );

    #[test]
    fn a_real_dolphin_selection_parses() {
        let paths = parse_uri_list(DOLPHIN_SAMPLE);
        assert_eq!(paths.len(), 4);
        assert_eq!(
            paths[0],
            PathBuf::from("/mnt/data/Pictures/netspeedmonitor  majorgeeks1.jpg"),
            "the doubled space should decode back to two spaces"
        );
        assert_eq!(
            paths[2],
            PathBuf::from("/mnt/data/Pictures/Snapshot@2022_0311_173322.jpg")
        );
        assert!(
            all_images(&paths),
            "a .gif alongside .jpg/.png is still images"
        );
    }

    #[test]
    fn a_list_of_images_is_recognised() {
        let paths = [PathBuf::from("/a/x.PNG"), PathBuf::from("/a/y.jpeg")];
        assert!(all_images(&paths), "extension matching must ignore case");
        assert!(!all_videos(&paths));
    }

    #[test]
    fn a_mixed_list_is_neither_images_nor_video() {
        let paths = [PathBuf::from("/a/x.png"), PathBuf::from("/a/y.txt")];
        assert!(!all_images(&paths));
        assert!(!all_videos(&paths));
    }

    #[test]
    fn an_empty_list_is_neither() {
        assert!(!all_images(&[]));
        assert!(!all_videos(&[]));
    }

    #[test]
    fn a_file_with_no_extension_is_neither() {
        let paths = [PathBuf::from("/a/README")];
        assert!(!all_images(&paths));
        assert!(!all_videos(&paths));
    }

    #[test]
    fn videos_are_recognised() {
        let paths = [PathBuf::from("/a/clip.MP4"), PathBuf::from("/a/b.mkv")];
        assert!(all_videos(&paths));
        assert!(!all_images(&paths));
    }

    #[test]
    fn descriptions_count_and_pluralise() {
        assert!(describe(&[PathBuf::from("/a/x.png")]).starts_with("1 file · images"));
        let two = [PathBuf::from("/a/x.png"), PathBuf::from("/a/y.png")];
        assert!(describe(&two).starts_with("2 files · images"));
    }

    #[test]
    fn a_description_survives_paths_that_do_not_exist() {
        // Metadata is unreadable here; the menu must still open.
        let missing = [PathBuf::from("/definitely/not/here.png")];
        assert_eq!(describe(&missing), "1 file · images");
    }

    #[test]
    fn extensions_are_collected_lowercase_and_deduplicated() {
        let paths = [
            PathBuf::from("/a/x.PNG"),
            PathBuf::from("/a/y.png"),
            PathBuf::from("/a/z.jpg"),
        ];
        let found = extensions(&paths);
        assert_eq!(found.len(), 2);
        assert!(found.contains("png") && found.contains("jpg"));
    }
}

//! End-to-end checks against the real system clipboard.
//!
//! Everything here writes to, and reads back from, whatever clipboard the
//! session actually has. That makes it the only test that exercises the path
//! the app really runs: arboard, the Wayland protocol, the facet model and the
//! built-in actions, together.
//!
//! All of it is `#[ignore]`d for that reason — it needs a desktop session, and
//! it clobbers the user's clipboard. Run it deliberately:
//!
//! ```bash
//! cargo test -p clip-convert-core --no-default-features --test clipboard_end_to_end \
//!     -- --ignored --test-threads=1
//! ```
//!
//! `--test-threads=1` is not optional: there is one clipboard, and two tests
//! racing for it would be testing each other.

use clipconv::action::{Action, ActionSpec, Builtin, InputMode, OutputMode};
use clipconv::clipboard;
use clipconv::config::Config;
use clipconv::content::ContentKind;
use clipconv::presets::{Fit, Preset};
use clipconv::replace::Replacement;
use clipconv::runner::{self, Prompt};
use std::path::{Path, PathBuf};

/// Waits for the desktop's clipboard manager to have finished with the
/// previous test's write.
///
/// On KDE the manager takes the selection over a moment after it is published,
/// and two writes inside that window can lose one. The app never writes twice
/// in a row like this; a test file that does needs to let it settle.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(800));
}

/// Answers whatever a built-in asks, so an action can run without a dialog.
#[derive(Default)]
struct Canned {
    limit: Option<usize>,
    target: Option<Preset>,
    replacement: Option<Replacement>,
    conversion: Option<String>,
}

impl Prompt for Canned {
    fn ask_limit(&self, _: &str, _: &str, _: usize) -> Option<usize> {
        self.limit
    }
    fn ask_resize_target(&self, _: &[Preset]) -> Option<Preset> {
        self.target.clone()
    }
    fn ask_replace(&self, _: &[String], _: &[String]) -> Option<Replacement> {
        self.replacement.clone()
    }
    fn ask_conversion(&self, _: &str, _: &[clipconv::protocol::ActionEntry]) -> Option<String> {
        self.conversion.clone()
    }
    fn ask_video_target(&self, _: &str) -> Option<clipconv::video::VideoTarget> {
        None
    }
}

fn action(id: &str, builtin: Builtin, when: &[&str]) -> Action {
    Action::from_spec(&ActionSpec {
        id: id.to_string(),
        label: id.to_string(),
        when: when.iter().map(|w| (*w).to_string()).collect(),
        builtin: Some(builtin),
        command: Vec::new(),
        input: InputMode::default(),
        output: OutputMode::default(),
        enabled: true,
        icon: None,
        button_color: None,
        text_color: None,
    })
    .expect("a valid action")
}

/// Writes a PNG of `width` x `height` to `path`, and returns its bytes.
fn png_file(path: &Path, width: u32, height: u32) -> Vec<u8> {
    let mut image = image::RgbaImage::new(width, height);
    // Not a flat colour: a single-colour image compresses to almost nothing,
    // which would make a size-cap test meaningless.
    let channel = |value: u32| u8::try_from(value % 256).unwrap_or(0);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        *pixel = image::Rgba([channel(x), channel(y), channel(x + y), 255]);
    }
    let bytes = clipconv::encode::encode(
        &image::DynamicImage::ImageRgba8(image),
        clipconv::encode::Format::Png,
        95,
    )
    .expect("encodes");
    std::fs::write(path, &bytes).expect("writes");
    bytes
}

#[test]
#[ignore = "uses the real clipboard"]
fn text_survives_a_round_trip() {
    settle();
    clipboard::write_text("hello from the test").expect("writes");
    let clip = clipboard::read().expect("reads");

    assert_eq!(clip.text(), Some("hello from the test"));
    assert!(clip.kinds().contains(&ContentKind::Text));
    assert_eq!(clipboard::read_text().expect("text"), "hello from the test");
}

#[test]
#[ignore = "uses the real clipboard"]
fn a_url_is_recognised_as_one() {
    settle();
    clipboard::write_text("https://example.com/a/b?c=d").expect("writes");
    let clip = clipboard::read().expect("reads");

    assert!(
        clip.kinds().contains(&ContentKind::Url),
        "{:?}",
        clip.kinds()
    );
    assert_eq!(
        clip.url().map(url::Url::to_string).as_deref(),
        Some("https://example.com/a/b?c=d")
    );
}

#[test]
#[ignore = "uses the real clipboard"]
fn an_image_comes_back_byte_for_byte() {
    settle();
    // The point of this one: reading must take the encoded bytes the clipboard
    // already holds rather than decoding and re-encoding them, which for a
    // large image cost minutes of CPU.
    let dir = tempfile::tempdir().expect("tempdir");
    let original = png_file(&dir.path().join("source.png"), 800, 600);

    clipboard::write_image("image/png", &original).expect("writes");
    let started = std::time::Instant::now();
    let clip = clipboard::read().expect("reads");
    let elapsed = started.elapsed();

    let (mime, bytes) = clip.image().expect("an image facet");
    assert_eq!(mime, "image/png");
    assert_eq!(
        bytes,
        original.as_slice(),
        "the image was re-encoded on the way in"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "reading took {elapsed:?}, which means it is doing real work on the pixels"
    );
}

#[test]
#[ignore = "uses the real clipboard"]
fn a_file_selection_is_files_and_images_and_text() {
    settle();
    let dir = tempfile::tempdir().expect("tempdir");
    let first = dir.path().join("one.png");
    let second = dir.path().join("two.png");
    png_file(&first, 64, 64);
    png_file(&second, 48, 48);

    clipboard::write_files(&[first.clone(), second.clone()]).expect("writes");
    let clip = clipboard::read().expect("reads");

    let kinds = clip.kinds();
    assert!(kinds.contains(&ContentKind::Files), "{kinds:?}");
    assert!(
        kinds.contains(&ContentKind::Image),
        "all-images should offer Image: {kinds:?}"
    );
    assert!(
        kinds.contains(&ContentKind::Text),
        "a selection is also its paths as text"
    );
    assert_eq!(clip.files(), Some([first, second].as_slice()));
}

#[test]
#[ignore = "uses the real clipboard"]
fn truncate_rewrites_the_clipboard() {
    settle();
    clipboard::write_text("0123456789abcdefghij").expect("writes");
    let clip = clipboard::read().expect("reads");

    let outcome = runner::run(
        &action("truncate", Builtin::Truncate, &["text"]),
        &clip,
        &Config::default(),
        &Canned {
            limit: Some(12),
            ..Canned::default()
        },
    )
    .expect("runs")
    .expect("an outcome");

    assert!(outcome.clipboard_changed);
    assert_eq!(clipboard::read_text().expect("text"), "012345678...");
}

#[test]
#[ignore = "uses the real clipboard"]
fn replace_rewrites_the_clipboard() {
    settle();
    clipboard::write_text("Smith, John").expect("writes");
    let clip = clipboard::read().expect("reads");

    runner::run(
        &action("replace", Builtin::Replace, &["text"]),
        &clip,
        &Config::default(),
        &Canned {
            replacement: Some(Replacement {
                pattern: r"(\w+), (\w+)".to_string(),
                replacement: "$2 $1".to_string(),
            }),
            ..Canned::default()
        },
    )
    .expect("runs")
    .expect("an outcome");

    assert_eq!(clipboard::read_text().expect("text"), "John Smith");
}

#[test]
#[ignore = "uses the real clipboard"]
fn resizing_a_clipboard_image_meets_its_cap() {
    settle();
    let dir = tempfile::tempdir().expect("tempdir");
    let original = png_file(&dir.path().join("big.png"), 1600, 1200);
    clipboard::write_image("image/png", &original).expect("writes");

    let clip = clipboard::read().expect("reads");
    let target = Config::default()
        .presets
        .into_iter()
        .find(|p| p.id == "whatsapp")
        .expect("the whatsapp preset");

    runner::run(
        &action("resize", Builtin::Resize, &["image"]),
        &clip,
        &Config::default(),
        &Canned {
            target: Some(target.clone()),
            ..Canned::default()
        },
    )
    .expect("runs")
    .expect("an outcome");

    let (_, bytes) = {
        let clip = clipboard::read().expect("reads back");
        let (mime, bytes) = clip.image().expect("an image");
        (mime.to_string(), bytes.to_vec())
    };
    assert!(
        bytes.len() as u64 <= target.max_bytes,
        "{} bytes is over the {} byte cap",
        bytes.len(),
        target.max_bytes
    );
    let decoded = image::load_from_memory(&bytes).expect("readable");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (512, 512),
        "exact canvas"
    );
}

#[test]
#[ignore = "uses the real clipboard"]
fn converting_a_clipboard_image_changes_its_format() {
    settle();
    let dir = tempfile::tempdir().expect("tempdir");
    let original = png_file(&dir.path().join("shot.png"), 300, 200);
    clipboard::write_image("image/png", &original).expect("writes");

    let clip = clipboard::read().expect("reads");
    runner::run(
        &action("convert", Builtin::Convert, &["image"]),
        &clip,
        &Config::default(),
        &Canned {
            conversion: Some("to-ico".to_string()),
            ..Canned::default()
        },
    )
    .expect("runs")
    .expect("an outcome");

    let clip = clipboard::read().expect("reads back");
    let (_, bytes) = clip.image().expect("an image");
    assert_eq!(
        image::guess_format(bytes).expect("a known format"),
        image::ImageFormat::Ico
    );
}

#[test]
#[ignore = "uses the real clipboard"]
fn resizing_a_selection_of_files_puts_the_results_back_as_files() {
    settle();
    let dir = tempfile::tempdir().expect("tempdir");
    let sources: Vec<PathBuf> = (0..3)
        .map(|i| {
            let path = dir.path().join(format!("shot{i}.png"));
            png_file(&path, 900, 700);
            path
        })
        .collect();

    clipboard::write_files(&sources).expect("writes");
    let clip = clipboard::read().expect("reads");

    let target = Preset {
        id: "test".to_string(),
        label: "Test".to_string(),
        width: 256,
        height: 256,
        max_bytes: 0,
        format: Some("png".to_string()),
        fit: Fit::Inside,
        icon: None,
        button_color: None,
        text_color: None,
    };
    runner::run(
        &action("resize", Builtin::Resize, &["image"]),
        &clip,
        &Config::default(),
        &Canned {
            target: Some(target),
            ..Canned::default()
        },
    )
    .expect("runs")
    .expect("an outcome");

    let clip = clipboard::read().expect("reads back");
    let written = clip.files().expect("the results are a file selection");
    assert_eq!(written.len(), 3);
    for path in written {
        let decoded = image::open(path).expect("each result is a readable image");
        assert!(
            decoded.width() <= 256 && decoded.height() <= 256,
            "{path:?}"
        );
    }
}

//! The resize action, for images and for video.
//!
//! Split out of `runner`, which was at the limit of what one file should hold.
//! Everything the action needs beyond the transformations themselves lives
//! here: which format the source is in, the single-image path, the batch path,
//! and the separate question video asks.

use crate::clipboard;
use crate::config::Config;
use crate::content::Clip;
use crate::cutout::ImageOptions;
use crate::image;
use crate::presets::Preset;
use crate::runner::{Outcome, Prompt, RunError};

pub(crate) fn run(
    clip: &Clip,
    config: &Config,
    prompt: &dyn Prompt,
) -> Result<Option<Outcome>, RunError> {
    // Video is a different question — length and bitrate, not a sticker
    // preset — so it gets its own form rather than a preset list that would
    // mean nothing for it.
    if clip.kinds().contains(&crate::content::ContentKind::Video) {
        return run_video_resize(clip, config, prompt);
    }

    let Some((target, options)) =
        prompt.ask_resize_target(&config.presets, source_keeps_alpha(clip))
    else {
        return Ok(None);
    };

    // Image data held directly on the clipboard is replaced in place. A
    // selection of image files is a batch, and its results are written out as
    // files because a clipboard can only hold one image at a time.
    if let Some((_, bytes)) = clip.image() {
        return resize_one(bytes, &target, options).map(Some);
    }

    let files = clip.files().ok_or_else(|| RunError::WrongKind {
        label: "Resize".to_string(),
    })?;

    let batch = crate::batch::resize_batch(files, &target, options)?;
    clipboard::write_files(&batch.written)?;
    Ok(Some(batch.into_outcome(&target)))
}

/// Whether the source is in a format that can hold transparency.
///
/// JPEG is the only format written here that cannot, so removing a background
/// for it would only be flattened back to white on the way out. A mixed
/// selection counts as yes: the files that can keep it should.
fn source_keeps_alpha(clip: &Clip) -> bool {
    if let Some((mime, _)) = clip.image() {
        return !matches!(
            mime.trim().to_ascii_lowercase().as_str(),
            "image/jpeg" | "image/jpg"
        );
    }
    clip.files().is_none_or(|files| {
        files.iter().any(|path| {
            path.extension()
                .and_then(|e| e.to_str())
                .is_none_or(|e| !matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg"))
        })
    })
}

/// Re-encodes a selection of videos to whatever the user asked for.
fn run_video_resize(
    clip: &Clip,
    config: &Config,
    prompt: &dyn Prompt,
) -> Result<Option<Outcome>, RunError> {
    let files = clip.files().ok_or_else(|| RunError::WrongKind {
        label: "Resize".to_string(),
    })?;

    let subject = format!(
        "{} {}",
        files.len(),
        clip.noun_for(crate::content::ContentKind::Video)
            .to_lowercase()
    );
    let Some(target) = prompt.ask_video_target(&subject) else {
        return Ok(None);
    };
    if target.constrains_nothing() {
        return Ok(None);
    }

    let batch = crate::batch::video_batch(files, &target, &config.video)?;
    clipboard::write_files(&batch.written)?;

    let mut outcome = batch.into_outcome(&Preset {
        id: "video".to_string(),
        label: target.summary(),
        width: 0,
        height: 0,
        max_bytes: 0,
        format: None,
        fit: crate::presets::Fit::Inside,
        icon: None,
        button_color: None,
        text_color: None,
    });
    outcome.message = outcome.message.replace("Resized", "Re-encoded");
    Ok(Some(outcome))
}

/// Asks what to convert to, then does it.
///
/// The choice is a second dialog rather than a longer action menu: which
/// conversions exist depends on what the clipboard holds, and folding them
/// into the first menu would make its length depend on the content in a way
/// that makes the common entries move around.
fn resize_one(bytes: &[u8], target: &Preset, options: ImageOptions) -> Result<Outcome, RunError> {
    let resized = image::resize(bytes, target, options)?;
    clipboard::write_image(&resized.mime, &resized.bytes)?;

    Ok(Outcome {
        message: format!(
            "Resized to {}×{} ({}) for {}.",
            resized.width,
            resized.height,
            crate::content::human_bytes(resized.bytes.len()),
            target.label
        ),
        clipboard_changed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jpeg_source_does_not_offer_to_remove_its_background() {
        // Nothing to hold the transparency: it would be flattened to white.
        let clip = Clip::from_image("image/jpeg".to_string(), vec![1, 2, 3]).expect("non-empty");
        assert!(!source_keeps_alpha(&clip));
    }

    #[test]
    fn a_png_source_does() {
        let clip = Clip::from_image("image/png".to_string(), vec![1, 2, 3]).expect("non-empty");
        assert!(source_keeps_alpha(&clip));
    }

    #[test]
    fn a_selection_counts_as_yes_unless_every_file_is_a_jpeg() {
        let jpegs = Clip::from_files(vec![
            std::path::PathBuf::from("/tmp/a.jpg"),
            std::path::PathBuf::from("/tmp/b.JPEG"),
        ])
        .expect("non-empty");
        assert!(!source_keeps_alpha(&jpegs));

        let mixed = Clip::from_files(vec![
            std::path::PathBuf::from("/tmp/a.jpg"),
            std::path::PathBuf::from("/tmp/b.png"),
        ])
        .expect("non-empty");
        assert!(
            source_keeps_alpha(&mixed),
            "the files that can keep transparency should"
        );
    }
}

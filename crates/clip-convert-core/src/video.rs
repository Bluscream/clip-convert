//! Re-encoding video to fit width, height, file size or length.
//!
//! Unlike images, this is not done in process. Every video crate on crates.io
//! is a binding to `FFmpeg` — there is no pure-Rust transcoder — and linking
//! `FFmpeg` would mean system headers on every platform the app builds on, for
//! one action. So the work is handed to the `ffmpeg` and `ffprobe` programs,
//! named in the config file so they can be pointed anywhere or swapped.
//!
//! Every field of a target is optional and only what is set is constrained,
//! which is the whole point: "make it fit 10 MB" and "make it 720p" are
//! different requests, and either may be made alone.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// What the user asked for. Every field is independent and optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VideoTarget {
    /// Widest the result may be, in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Tallest the result may be, in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Upper bound on the encoded file size, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Longest the result may run, in seconds. The video is cut, not sped up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
}

impl VideoTarget {
    /// Whether this asks for anything at all.
    #[must_use]
    pub fn constrains_nothing(&self) -> bool {
        self.width.is_none()
            && self.height.is_none()
            && self.max_bytes.is_none()
            && self.seconds.is_none()
    }

    /// A one-line description of what was asked for, for the notification.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        match (self.width, self.height) {
            (Some(w), Some(h)) => parts.push(format!("{w}×{h}")),
            (Some(w), None) => parts.push(format!("{w} wide")),
            (None, Some(h)) => parts.push(format!("{h} tall")),
            (None, None) => {}
        }
        if let Some(max) = self.max_bytes {
            parts.push(format!(
                "max {}",
                crate::content::human_bytes(usize::try_from(max).unwrap_or(usize::MAX))
            ));
        }
        if let Some(seconds) = self.seconds {
            parts.push(format!("first {}", format_seconds(seconds)));
        }
        if parts.is_empty() {
            "unchanged".to_string()
        } else {
            parts.join(" · ")
        }
    }

    /// How long the result will run, given how long the source does.
    #[must_use]
    pub fn output_seconds(&self, source_seconds: f64) -> f64 {
        match self.seconds {
            Some(limit) if limit < source_seconds => limit,
            _ => source_seconds,
        }
    }
}

/// Where the external programs are and how they should encode.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VideoSettings {
    /// The encoder to run. A bare name is looked up on `PATH`.
    #[serde(default = "default_ffmpeg")]
    pub ffmpeg: String,
    /// The program that reports a video's duration, needed only when a file
    /// size is asked for.
    #[serde(default = "default_ffprobe")]
    pub ffprobe: String,
    /// Video codec passed to `-c:v`.
    #[serde(default = "default_codec")]
    pub codec: String,
    /// Audio bitrate, in kbit/s. Subtracted from the budget when sizing.
    #[serde(default = "default_audio_bitrate")]
    pub audio_bitrate_kbps: u32,
    /// Container for the result, as a bare extension.
    #[serde(default = "default_container")]
    pub container: String,
    /// Anything else to pass to the encoder, inserted before the output path.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

fn default_ffmpeg() -> String {
    "ffmpeg".to_string()
}
fn default_ffprobe() -> String {
    "ffprobe".to_string()
}
fn default_codec() -> String {
    "libx264".to_string()
}
fn default_container() -> String {
    "mp4".to_string()
}
const fn default_audio_bitrate() -> u32 {
    128
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            ffmpeg: default_ffmpeg(),
            ffprobe: default_ffprobe(),
            codec: default_codec(),
            audio_bitrate_kbps: default_audio_bitrate(),
            container: default_container(),
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VideoError {
    #[error(
        "a file size can only be met if the video's length is known, and it could not be read"
    )]
    UnknownDuration,
    #[error("{size} leaves nothing for the picture once {audio} kbit/s of audio is accounted for")]
    BudgetTooSmall { size: String, audio: u32 },
}

/// The arguments that ask `ffprobe` how long a video runs.
#[must_use]
pub fn probe_args(settings: &VideoSettings, source: &Path) -> Vec<String> {
    vec![
        settings.ffprobe.clone(),
        "-v".to_string(),
        "error".to_string(),
        "-show_entries".to_string(),
        "format=duration".to_string(),
        "-of".to_string(),
        "default=noprint_wrappers=1:nokey=1".to_string(),
        source.display().to_string(),
    ]
}

/// Reads a duration in seconds out of `ffprobe`'s output.
#[must_use]
pub fn parse_duration(output: &str) -> Option<f64> {
    output
        .lines()
        .find_map(|line| line.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
}

/// The video bitrate, in kbit/s, that fits `max_bytes` over `seconds`.
///
/// # Errors
///
/// Returns [`VideoError::BudgetTooSmall`] when the audio alone would already
/// overrun the budget, which is a question to put back to the user rather than
/// an encode to attempt and fail.
pub fn video_bitrate_kbps(
    max_bytes: u64,
    seconds: f64,
    audio_kbps: u32,
) -> Result<u32, VideoError> {
    #[allow(clippy::cast_precision_loss)] // A file size never needs 53 bits here.
    let total_kbps = (max_bytes as f64 * 8.0 / 1000.0) / seconds;
    let video_kbps = total_kbps - f64::from(audio_kbps);

    // A little below the budget: containers carry overhead, and an encoder
    // asked for exactly the limit lands just over it often enough to matter.
    let video_kbps = video_kbps * 0.95;

    if !video_kbps.is_finite() || video_kbps < 1.0 {
        return Err(VideoError::BudgetTooSmall {
            size: crate::content::human_bytes(usize::try_from(max_bytes).unwrap_or(usize::MAX)),
            audio: audio_kbps,
        });
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    // Guarded above: finite and at least 1.
    Ok(video_kbps as u32)
}

/// The arguments that re-encode one video to `target`.
///
/// `source_seconds` is needed only when a file size was asked for.
///
/// # Errors
///
/// Returns [`VideoError::UnknownDuration`] if a size was asked for without a
/// known length, or [`VideoError::BudgetTooSmall`] if the size cannot be met.
pub fn encode_args(
    settings: &VideoSettings,
    source: &Path,
    output: &Path,
    target: &VideoTarget,
    source_seconds: Option<f64>,
) -> Result<Vec<String>, VideoError> {
    let mut args = vec![
        settings.ffmpeg.clone(),
        // Overwrite without asking: the output is a fresh scratch path, and
        // there is no terminal to answer the prompt on.
        "-y".to_string(),
        "-i".to_string(),
        source.display().to_string(),
    ];

    if let Some(seconds) = target.seconds {
        args.push("-t".to_string());
        args.push(format!("{seconds:.3}"));
    }
    if let Some(scale) = scale_filter(target) {
        args.push("-vf".to_string());
        args.push(scale);
    }
    if let Some(max_bytes) = target.max_bytes {
        let seconds = source_seconds
            .map(|source| target.output_seconds(source))
            .ok_or(VideoError::UnknownDuration)?;
        let kbps = video_bitrate_kbps(max_bytes, seconds, settings.audio_bitrate_kbps)?;
        // maxrate and bufsize together stop the encoder spending its whole
        // budget on an early busy scene and overrunning the file size.
        args.extend([
            "-b:v".to_string(),
            format!("{kbps}k"),
            "-maxrate".to_string(),
            format!("{kbps}k"),
            "-bufsize".to_string(),
            format!("{}k", kbps * 2),
        ]);
    }

    args.extend([
        "-c:v".to_string(),
        settings.codec.clone(),
        "-b:a".to_string(),
        format!("{}k", settings.audio_bitrate_kbps),
    ]);
    args.extend(settings.extra_args.iter().cloned());
    args.push(output.display().to_string());
    Ok(args)
}

/// The `-vf` value that fits the picture in the box, or `None` if neither axis
/// was constrained.
///
/// An unset axis becomes `-1`, which tells the scaler to keep the aspect ratio;
/// `-2` rather than `-1` because most codecs require even dimensions.
fn scale_filter(target: &VideoTarget) -> Option<String> {
    let (width, height) = (target.width, target.height);
    if width.is_none() && height.is_none() {
        return None;
    }

    let axis = |value: Option<u32>| value.map_or("-2".to_string(), |v| v.to_string());
    let filter = format!("scale={}:{}", axis(width), axis(height));

    // With both axes given the box is a maximum, not a stretch: the picture is
    // fitted inside it and padded by nothing.
    if width.is_some() && height.is_some() {
        return Some(format!("{filter}:force_original_aspect_ratio=decrease"));
    }
    Some(filter)
}

/// Formats a duration the way it was most likely typed.
#[must_use]
pub fn format_seconds(seconds: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let whole = seconds.max(0.0).round() as u64;
    if whole < 60 {
        return format!("{whole}s");
    }
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// Reads a length written as `90`, `90s`, `1:30` or `1m30s`.
#[must_use]
pub fn parse_seconds(text: &str) -> Option<f64> {
    let text = text.trim().to_ascii_lowercase();
    if text.is_empty() {
        return None;
    }

    // `1:30` and `1:02:03`, most significant part first.
    if text.contains(':') {
        let mut total = 0.0;
        for part in text.split(':') {
            total = total * 60.0 + part.trim().parse::<f64>().ok()?;
        }
        return (total > 0.0).then_some(total);
    }

    // `1m30s`, `90s`, `2h`.
    let mut total = 0.0;
    let mut number = String::new();
    let mut saw_unit = false;
    for c in text.chars() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        let value: f64 = number.parse().ok()?;
        number.clear();
        saw_unit = true;
        total += value
            * match c {
                'h' => 3600.0,
                'm' => 60.0,
                's' => 1.0,
                _ => return None,
            };
    }
    if !number.is_empty() {
        total += number.parse::<f64>().ok()?;
    } else if !saw_unit {
        return None;
    }

    (total > 0.0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn target() -> VideoTarget {
        VideoTarget::default()
    }

    fn args_for(target: &VideoTarget, seconds: Option<f64>) -> Vec<String> {
        encode_args(
            &VideoSettings::default(),
            &PathBuf::from("/in.mov"),
            &PathBuf::from("/out.mp4"),
            target,
            seconds,
        )
        .expect("buildable")
    }

    #[test]
    fn an_empty_target_asks_for_nothing() {
        assert!(target().constrains_nothing());
        assert_eq!(target().summary(), "unchanged");
        assert!(!VideoTarget {
            seconds: Some(30.0),
            ..target()
        }
        .constrains_nothing());
    }

    #[test]
    fn only_what_was_set_reaches_the_encoder() {
        let args = args_for(&target(), None);
        assert!(!args.iter().any(|a| a == "-vf"), "{args:?}");
        assert!(!args.iter().any(|a| a == "-t"), "{args:?}");
        assert!(!args.iter().any(|a| a == "-b:v"), "{args:?}");
        assert_eq!(args.last().map(String::as_str), Some("/out.mp4"));
    }

    #[test]
    fn one_axis_alone_keeps_the_aspect_ratio() {
        let args = args_for(
            &VideoTarget {
                width: Some(1280),
                ..target()
            },
            None,
        );
        let filter = args
            .iter()
            .position(|a| a == "-vf")
            .and_then(|at| args.get(at + 1))
            .expect("a scale filter");
        assert_eq!(filter, "scale=1280:-2", "an unset axis must follow along");
    }

    #[test]
    fn both_axes_are_a_box_rather_than_a_stretch() {
        let args = args_for(
            &VideoTarget {
                width: Some(1280),
                height: Some(720),
                ..target()
            },
            None,
        );
        assert!(
            args.iter()
                .any(|a| a == "scale=1280:720:force_original_aspect_ratio=decrease"),
            "{args:?}"
        );
    }

    #[test]
    fn a_length_limit_cuts_rather_than_speeds_up() {
        let args = args_for(
            &VideoTarget {
                seconds: Some(30.0),
                ..target()
            },
            None,
        );
        let at = args.iter().position(|a| a == "-t").expect("-t");
        assert_eq!(args[at + 1], "30.000");
    }

    #[test]
    fn a_size_limit_becomes_a_bitrate_from_the_length() {
        // 10 MB over 100 seconds is 800 kbit/s in total, less 128 of audio.
        let kbps = video_bitrate_kbps(10 * 1000 * 1000, 100.0, 128).expect("fits");
        assert!((630..=640).contains(&kbps), "got {kbps}");
    }

    #[test]
    fn the_budget_is_taken_from_the_cut_length_not_the_original() {
        let target = VideoTarget {
            max_bytes: Some(10 * 1000 * 1000),
            seconds: Some(50.0),
            ..target()
        };
        // The source runs 200s but only 50 will be kept, so the bitrate may be
        // four times what the full length would allow.
        assert!((target.output_seconds(200.0) - 50.0).abs() < f64::EPSILON);
        let args = args_for(&target, Some(200.0));
        let at = args.iter().position(|a| a == "-b:v").expect("-b:v");
        assert_eq!(args[at + 1], "1398k");
    }

    #[test]
    fn a_size_that_cannot_hold_the_audio_is_refused_with_a_reason() {
        let err = video_bitrate_kbps(1024, 600.0, 128).expect_err("impossible");
        assert!(matches!(err, VideoError::BudgetTooSmall { .. }), "{err}");
    }

    #[test]
    fn a_size_limit_without_a_known_length_is_refused_rather_than_guessed() {
        let err = encode_args(
            &VideoSettings::default(),
            &PathBuf::from("/in.mov"),
            &PathBuf::from("/out.mp4"),
            &VideoTarget {
                max_bytes: Some(1_000_000),
                ..target()
            },
            None,
        )
        .expect_err("no duration");
        assert_eq!(err, VideoError::UnknownDuration);
    }

    #[test]
    fn a_duration_is_read_out_of_ffprobes_output() {
        assert_eq!(parse_duration("123.456\n"), Some(123.456));
        assert_eq!(parse_duration("N/A"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("0.000"), None, "a zero length is no length");
    }

    #[test]
    fn a_length_can_be_written_the_ways_people_write_lengths() {
        assert_eq!(parse_seconds("90"), Some(90.0));
        assert_eq!(parse_seconds("90s"), Some(90.0));
        assert_eq!(parse_seconds("1:30"), Some(90.0));
        assert_eq!(parse_seconds("1m30s"), Some(90.0));
        assert_eq!(parse_seconds("1:02:03"), Some(3723.0));
        assert_eq!(parse_seconds("2h"), Some(7200.0));
        assert_eq!(parse_seconds(""), None);
        assert_eq!(parse_seconds("soon"), None);
        assert_eq!(parse_seconds("0"), None);
    }

    #[test]
    fn a_length_is_shown_the_way_it_was_probably_typed() {
        assert_eq!(format_seconds(45.0), "45s");
        assert_eq!(format_seconds(90.0), "1:30");
        assert_eq!(format_seconds(3600.0), "60:00");
    }

    #[test]
    fn a_summary_mentions_only_what_was_asked_for() {
        let target = VideoTarget {
            width: Some(1280),
            height: Some(720),
            max_bytes: Some(10 * 1024 * 1024),
            seconds: None,
        };
        assert_eq!(target.summary(), "1280×720 · max 10.0 MB");
    }

    #[test]
    fn the_probe_asks_only_for_the_duration() {
        let args = probe_args(&VideoSettings::default(), &PathBuf::from("/in.mov"));
        assert_eq!(args.first().map(String::as_str), Some("ffprobe"));
        assert!(args.iter().any(|a| a == "format=duration"), "{args:?}");
        assert_eq!(args.last().map(String::as_str), Some("/in.mov"));
    }
}

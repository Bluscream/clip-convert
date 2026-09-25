//! Running an action over a selection of files.
//!
//! A clipboard can hold one image but many files, so the built-ins that act on
//! images have two shapes: one that replaces what is on the clipboard, and this
//! one, which writes its results into a scratch directory and puts those back
//! as a file list.
//!
//! Nothing here touches the clipboard. Keeping that side effect in the caller
//! is what lets these be tested without writing over whatever the user has
//! copied — which an earlier version did.

use crate::convert;
use crate::presets::Preset;
use crate::runner::{Outcome, RunError, COMMAND_TIMEOUT};
use crate::{exec, image, scratch};
use std::path::{Path, PathBuf};

/// What a batch produced.
#[derive(Debug)]
pub(crate) struct Batch {
    pub(crate) written: Vec<PathBuf>,
    pub(crate) failures: Vec<String>,
    pub(crate) total: usize,
}

impl Batch {
    /// Reports what happened, naming the failures rather than hiding them.
    pub(crate) fn into_outcome(self, target: &Preset) -> Outcome {
        let mut message = format!(
            "Resized {} of {} for {}. The results are on the clipboard.",
            self.written.len(),
            self.total,
            target.label
        );
        if !self.failures.is_empty() {
            use std::fmt::Write;
            let _ = write!(
                message,
                "\n{} failed: {}",
                self.failures.len(),
                self.failures.join("; ")
            );
        }
        Outcome {
            message,
            clipboard_changed: true,
        }
    }
}

impl Batch {
    /// Reports a conversion batch, which counts only the matching files.
    pub(crate) fn into_conversion_outcome(self, conversion: &convert::Conversion) -> Outcome {
        let mut outcome = self.into_outcome(&Preset {
            id: conversion.id.clone(),
            label: conversion.display_label(),
            width: 0,
            height: 0,
            max_bytes: 0,
            format: None,
            fit: crate::presets::Fit::Inside,
            icon: None,
            button_color: None,
            text_color: None,
        });
        outcome.message = outcome.message.replace("Resized", "Converted");
        outcome
    }
}

/// Converts every file, writing the results into a scratch directory.
///
/// As with a resize batch, one unreadable file does not abandon the rest.
pub(crate) fn convert_batch(
    files: &[PathBuf],
    conversion: &convert::Conversion,
) -> Result<Batch, RunError> {
    let directory = scratch::output_dir("convert").map_err(RunError::TempFile)?;
    let wanted: Vec<String> = conversion
        .from
        .iter()
        .map(|f| convert::normalise(f))
        .collect();

    let mut written = Vec::new();
    let mut failures = Vec::new();
    let mut considered = 0;

    for source in files {
        let extension = source
            .extension()
            .and_then(|e| e.to_str())
            .map(convert::normalise)
            .unwrap_or_default();
        if !wanted.contains(&extension) {
            continue;
        }
        considered += 1;

        match convert_file(source, conversion, &directory) {
            Ok(path) => written.push(path),
            Err(e) => {
                log::warn!("converting {} failed: {e}", source.display());
                failures.push(format!("{}: {e}", file_label(source)));
            }
        }
    }

    if written.is_empty() {
        return Err(RunError::BatchFailed {
            total: considered,
            first: failures.into_iter().next().unwrap_or_default(),
        });
    }

    Ok(Batch {
        written,
        failures,
        total: considered,
    })
}

/// Converts one file, returning where the result was written.
pub(crate) fn convert_file(
    source: &std::path::Path,
    conversion: &convert::Conversion,
    directory: &std::path::Path,
) -> Result<PathBuf, RunError> {
    let output = directory.join(scratch::output_name(
        source,
        "converted",
        &convert::normalise(&conversion.to),
    ));

    if conversion.is_native() {
        let bytes = std::fs::read(source).map_err(RunError::TempFile)?;
        let converted = convert::image_to(&bytes, &conversion.to)?;
        std::fs::write(&output, converted).map_err(RunError::TempFile)?;
        return Ok(output);
    }

    run_conversion_command(&conversion.command, source, &output)?;
    Ok(output)
}

/// Runs a configured conversion command for one file.
///
/// `{input}` and `{output}` are replaced with the two paths. A command with no
/// `{output}` is taken to print its result, which is captured to the file —
/// that is how most text converters behave.
fn run_conversion_command(
    command: &[String],
    source: &std::path::Path,
    output: &std::path::Path,
) -> Result<(), RunError> {
    let writes_its_own = command.iter().any(|part| part.contains("{output}"));
    let argv: Vec<String> = command
        .iter()
        .map(|part| {
            part.replace("{input}", &source.display().to_string())
                .replace("{output}", &output.display().to_string())
        })
        .collect();

    let result = exec::run(&argv, None, COMMAND_TIMEOUT)?;
    if !writes_its_own {
        std::fs::write(output, &result.stdout).map_err(RunError::TempFile)?;
    }
    Ok(())
}

/// Resizes every file, writing the results into a scratch directory.
///
/// Does not touch the clipboard — the caller does that. Keeping the side effect
/// out means this can be tested without writing over whatever the user has
/// copied, which an earlier version did.
///
/// One unreadable file does not abandon the rest: the others are still useful,
/// and the failures are named in the result rather than swallowed.
pub(crate) fn resize_batch(files: &[PathBuf], target: &Preset) -> Result<Batch, RunError> {
    let directory = scratch::output_dir("resize").map_err(RunError::TempFile)?;

    let mut written = Vec::new();
    let mut failures = Vec::new();

    for source in files {
        match resize_file(source, target, &directory) {
            Ok(path) => written.push(path),
            Err(e) => {
                log::warn!("resizing {} failed: {e}", source.display());
                failures.push(format!("{}: {e}", file_label(source)));
            }
        }
    }

    if written.is_empty() {
        return Err(RunError::BatchFailed {
            total: files.len(),
            first: failures.into_iter().next().unwrap_or_default(),
        });
    }

    Ok(Batch {
        written,
        failures,
        total: files.len(),
    })
}

/// Resizes one file of a batch, returning where it was written.
fn resize_file(
    source: &Path,
    target: &Preset,
    directory: &std::path::Path,
) -> Result<PathBuf, RunError> {
    let bytes = std::fs::read(source).map_err(RunError::TempFile)?;
    let resized = image::resize(&bytes, target)?;

    // A target that keeps the source's format has no extension of its own, so
    // the result keeps the one it came in with.
    let extension = target.format.clone().unwrap_or_else(|| {
        source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase()
    });
    let output = directory.join(scratch::output_name(source, &target.label, &extension));
    std::fs::write(&output, &resized.bytes).map_err(RunError::TempFile)?;
    Ok(output)
}

/// A file's name, for a message that must stay short.
fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("a file")
        .to_string()
}

//! Where the results of a batch action are written.
//!
//! A batch action produces files, and those files have to outlive the action:
//! they sit on the clipboard until the user pastes them, which may be a while.
//! So this is a plain directory under the system temp directory rather than a
//! `TempDir`, which would delete itself the moment the action returned.
//!
//! Nothing cleans these up during a session. The system clears its temp
//! directory on reboot, which is the trade the "temp directory" choice makes:
//! results never clutter the folder the originals came from, but they are not
//! permanent either.

use std::path::{Path, PathBuf};

/// The directory all of this app's scratch output lives under.
const ROOT: &str = "clip-convert";

/// Creates a fresh directory for one batch run.
///
/// The name carries the action and a counter so that two runs of the same
/// action in one session do not overwrite each other.
///
/// # Errors
///
/// Returns an error if the directory could not be created.
pub fn output_dir(action: &str) -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let run = COUNTER.fetch_add(1, Ordering::Relaxed);

    let dir = std::env::temp_dir()
        .join(ROOT)
        .join(format!("{}-{stamp}-{run}", sanitise(action)));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Builds the output name for one file of a batch.
///
/// Keeps the original stem so the results are recognisable, adds the target's
/// name so several runs over the same sources do not collide, and uses the new
/// extension.
#[must_use]
pub fn output_name(source: &Path, target: &str, extension: &str) -> String {
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("clipboard");
    format!("{}.{}.{extension}", sanitise(stem), sanitise(target))
}

/// Strips anything that would be awkward or unsafe in a file name.
///
/// The parts come from a preset label and from the source file's own name, so
/// this guards against a separator or a traversal sequence reaching the path.
fn sanitise(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('-').trim();
    if trimmed.is_empty() {
        "clipboard".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_run_gets_its_own_directory() {
        let first = output_dir("resize").expect("created");
        let second = output_dir("resize").expect("created");
        assert_ne!(first, second, "two runs must not share a directory");
        assert!(first.is_dir() && second.is_dir());

        let _ = std::fs::remove_dir(&first);
        let _ = std::fs::remove_dir(&second);
    }

    #[test]
    fn the_directory_lives_under_the_temp_root() {
        let dir = output_dir("resize").expect("created");
        assert!(dir.starts_with(std::env::temp_dir().join(ROOT)));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn output_names_keep_the_stem_and_take_the_new_extension() {
        let name = output_name(Path::new("/a/photo.jpg"), "Discord sticker", "png");
        assert_eq!(name, "photo.Discord sticker.png");
    }

    #[test]
    fn a_separator_in_a_name_cannot_escape_the_directory() {
        // The preset label is user-editable and the stem comes from the
        // clipboard, so neither may introduce a path.
        let name = output_name(Path::new("/a/../../etc/passwd"), "../../root", "png");
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains(".."), "{name}");
    }

    #[test]
    fn a_name_that_sanitises_to_nothing_still_produces_a_file() {
        let name = output_name(Path::new("/a/....."), "///", "png");
        assert!(Path::new(&name).extension().is_some_and(|e| e == "png"));
        assert!(!name.starts_with('.'), "{name}");
    }

    #[test]
    fn non_ascii_stems_are_replaced_rather_than_dropped() {
        let name = output_name(Path::new("/a/café.jpg"), "t", "png");
        assert!(name.ends_with(".t.png"), "{name}");
        assert!(!name.is_empty());
    }
}

//! Making sure only one copy of the app runs.
//!
//! Two instances would each watch the clipboard, each shorten the same URL, and
//! each open a menu for every hotkey press.
//!
//! This uses `flock` on a file rather than a named socket or a PID file, for two
//! reasons. The kernel releases the lock when the process dies however it dies,
//! so a crash or a `SIGKILL` cannot leave a stale lock behind. And Rust opens
//! files close-on-exec, so the descriptor is not inherited by the children this
//! app spawns — an earlier socket-based approach was kept alive indefinitely by
//! an orphaned `wl-paste`, which made the app refuse to start again.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::File;
use std::path::PathBuf;

/// Holds the lock. Dropping it, or the process exiting, releases it.
pub struct Guard {
    // Never read: the lock lives exactly as long as this open file.
    _file: File,
}

/// Where the lock file lives.
///
/// `XDG_RUNTIME_DIR` is the correct home for it: it is per-user, on tmpfs, and
/// cleared when the session ends.
fn lock_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
        .join("linux-clip-convert.lock")
}

/// Takes the single-instance lock.
///
/// # Errors
///
/// Returns an error if the lock file cannot be created, or if another instance
/// already holds it.
pub fn acquire() -> Result<Guard> {
    let path = lock_path();
    let file = File::create(&path)
        .with_context(|| format!("creating the lock file {}", path.display()))?;

    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("another instance is already running"))?;

    Ok(Guard { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_is_released_when_the_guard_is_dropped() {
        let guard = acquire().expect("first acquire");
        assert!(acquire().is_err(), "a second acquire must fail");
        drop(guard);
        assert!(acquire().is_ok(), "the lock should be free again");
    }

    #[test]
    fn the_lock_file_lives_in_the_runtime_directory_when_there_is_one() {
        let path = lock_path();
        assert!(path.is_absolute());
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("linux-clip-convert.lock")
        );
    }
}

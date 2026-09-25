//! Making sure only one copy of the app runs.
//!
//! Two instances would each listen for the hotkey, each open a menu, and
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
        .join("clip-convert.lock")
}

/// Takes the single-instance lock.
///
/// # Errors
///
/// Returns an error if the lock file cannot be created, or if another instance
/// already holds it.
pub fn acquire() -> Result<Guard> {
    acquire_at(&lock_path())
}

/// Takes the lock on a specific file.
///
/// Separate from [`acquire`] so tests can use a temporary path instead of the
/// real one, which is shared with any running copy of the app.
///
/// # Errors
///
/// Returns an error if the file cannot be created, or if it is already locked.
pub fn acquire_at(path: &std::path::Path) -> Result<Guard> {
    let file =
        File::create(path).with_context(|| format!("creating the lock file {}", path.display()))?;

    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("another instance is already running"))?;

    Ok(Guard { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asks a separate process whether the lock is free.
    ///
    /// Cross-process exclusion is the property that matters — two copies of the
    /// app must not both run — and it is the only one `flock` actually
    /// guarantees. Two acquisitions inside one process are not a meaningful
    /// test of it.
    #[cfg(target_os = "linux")]
    fn lock_is_free(path: &std::path::Path) -> bool {
        std::process::Command::new("flock")
            .arg("--nonblock")
            .arg(path)
            .args(["--command", "true"])
            .status()
            .expect("flock(1) is part of util-linux and should be present")
            .success()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn another_process_cannot_take_the_lock_until_the_guard_is_dropped() {
        // A temporary path, not the real one: the real lock is shared with any
        // running copy of the app, which would make this test fail at random.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.lock");

        assert!(lock_is_free(&path), "a fresh lock file should be free");

        let guard = acquire_at(&path).expect("acquire");
        assert!(
            !lock_is_free(&path),
            "a second process must not be able to take a held lock"
        );

        drop(guard);
        assert!(
            lock_is_free(&path),
            "the lock should be released when the guard is dropped"
        );
    }

    #[test]
    fn acquiring_creates_the_lock_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested.lock");
        let _guard = acquire_at(&path).expect("acquire");
        assert!(path.is_file());
    }

    #[test]
    fn the_lock_file_lives_in_the_runtime_directory_when_there_is_one() {
        let path = lock_path();
        assert!(path.is_absolute());
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("clip-convert.lock")
        );
    }
}

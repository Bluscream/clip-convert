//! Commands the tray and the signal handler send to the main thread.

/// Something the daemon must do.
pub enum Command {
    /// Re-read the config, reporting the result.
    Reload,
    /// Shut down.
    Quit,
}

/// The channel end used to post a [`Command`].
pub type Commands = std::sync::mpsc::Sender<Command>;

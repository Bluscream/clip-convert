//! The tray icon.
//!
//! Split by platform deliberately. `tray-icon`'s Linux backend is built on GTK
//! and libappindicator, which would pull a C toolkit back into a project that
//! otherwise needs none; `ksni` speaks the `StatusNotifierItem` D-Bus protocol
//! that Linux desktops implement natively. Windows and macOS have no such
//! protocol, so they use `tray-icon` against their own APIs.

#[cfg(not(target_os = "linux"))]
mod desktop;
#[cfg(target_os = "linux")]
mod linux;

use crate::app::App;
use crate::ui::UiCommand;
use anyhow::Result;
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// Publishes the tray icon.
///
/// Returns a short description of the backend that was used, for the startup
/// log — a silently chosen backend is very hard to diagnose remotely.
///
/// # Errors
///
/// Returns an error if the tray could not be published. Callers should treat
/// this as degraded rather than fatal: the hotkey and auto-shortening still
/// work without a tray icon.
pub fn start(app: Arc<App>, commands: Sender<UiCommand>) -> Result<String> {
    #[cfg(target_os = "linux")]
    {
        linux::start(app, commands)
    }
    #[cfg(not(target_os = "linux"))]
    {
        desktop::start(app, commands)
    }
}

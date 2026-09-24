//! The tray icon, as a `StatusNotifierItem`.
//!
//! `ksni` speaks the `StatusNotifierItem` protocol over D-Bus directly, which is
//! what KDE implements natively and what libappindicator is a wrapper around.
//! Using it avoids linking any C tray library, and so avoids the mismatch
//! between the appindicator and Ayatana forks that different distributions ship.

use crate::app::App;
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// Things the tray asks the main thread to do.
pub enum TrayCommand {
    Reload,
    Quit,
}

pub struct Tray {
    app: Arc<App>,
    commands: Sender<TrayCommand>,
}

impl Tray {
    #[must_use]
    pub fn new(app: Arc<App>, commands: Sender<TrayCommand>) -> Self {
        Self { app, commands }
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "linux-clip-convert".to_string()
    }

    fn title(&self) -> String {
        "Clip Convert".to_string()
    }

    fn icon_name(&self) -> String {
        // A themed name rather than a bundled pixmap: it follows the user's icon
        // theme and stays sharp at any panel size. The state is still visible,
        // because the name changes with it.
        if self.app.auto_shorten() {
            "edit-link".to_string()
        } else {
            "edit-paste".to_string()
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let state = if self.app.auto_shorten() {
            "Auto-shorten is on"
        } else {
            "Auto-shorten is off"
        };
        ksni::ToolTip {
            title: "Clip Convert".to_string(),
            description: state.to_string(),
            icon_name: self.icon_name(),
            icon_pixmap: Vec::new(),
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};

        vec![
            CheckmarkItem {
                label: "Auto-shorten copied links".to_string(),
                checked: self.app.auto_shorten(),
                activate: Box::new(|tray: &mut Self| {
                    let now_on = !tray.app.auto_shorten();
                    tray.app.set_auto_shorten(now_on);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Edit configuration…".to_string(),
                icon_name: "document-edit".to_string(),
                activate: Box::new(|tray: &mut Self| {
                    let path = tray.app.config_path().to_path_buf();
                    // Detached: the editor outlives this callback, and the tray
                    // must not block while it is open.
                    match std::process::Command::new("xdg-open").arg(&path).spawn() {
                        Ok(_) => log::debug!("opened {}", path.display()),
                        Err(e) => log::warn!("could not open {}: {e}", path.display()),
                    }
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Reload configuration".to_string(),
                icon_name: "view-refresh".to_string(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(TrayCommand::Reload);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".to_string(),
                icon_name: "application-exit".to_string(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

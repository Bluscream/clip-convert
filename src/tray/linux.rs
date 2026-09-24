//! The tray icon on Linux, as a `StatusNotifierItem`.

use crate::app::App;
use crate::ui::UiCommand;
use anyhow::{Context, Result};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

/// How long to wait for a tray host to answer before carrying on without one.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(10);

struct Tray {
    app: Arc<App>,
    commands: Sender<UiCommand>,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "clip-convert".to_string()
    }

    fn title(&self) -> String {
        "Clip Convert".to_string()
    }

    fn icon_name(&self) -> String {
        // A themed name rather than a bundled pixmap: it follows the user's icon
        // theme and stays sharp at any panel size. The state stays visible
        // because the name changes with it.
        if self.app.auto_shorten() {
            "edit-link".to_string()
        } else {
            "edit-paste".to_string()
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Clip Convert".to_string(),
            description: if self.app.auto_shorten() {
                "Auto-shorten is on".to_string()
            } else {
                "Auto-shorten is off".to_string()
            },
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
                    let _ = tray.commands.send(UiCommand::Reload);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".to_string(),
                icon_name: "application-exit".to_string(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(UiCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Publishes the tray on a small runtime of its own.
pub fn start(app: Arc<App>, commands: Sender<UiCommand>) -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the tray runtime")?;

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        runtime.block_on(async move {
            use ksni::TrayMethods;
            let tray = Tray { app, commands };
            match tray.spawn().await {
                Ok(handle) => {
                    let _ = ready_tx.send(Ok(()));
                    // Keeps the runtime — and with it the tray — alive. The
                    // handle must not be dropped or the item disappears.
                    std::future::pending::<()>().await;
                    drop(handle);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("{e}")));
                }
            }
        });
    });

    match ready_rx.recv_timeout(PUBLISH_TIMEOUT) {
        Ok(Ok(())) => Ok("StatusNotifierItem (ksni)".to_string()),
        Ok(Err(e)) => anyhow::bail!("could not publish the tray icon: {e}"),
        // No StatusNotifierWatcher is degraded, not fatal.
        Err(_) => anyhow::bail!("no tray host responded"),
    }
}

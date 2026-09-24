//! The tray icon on Linux, as a `StatusNotifierItem`.

use crate::app::App;
use crate::command::{Command, Commands};
use anyhow::{Context, Result};
use clipconv::content::ContentKind;
use std::sync::Arc;
use std::time::Duration;

/// How long to wait for a tray host to answer before carrying on without one.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(10);

struct Tray {
    app: Arc<App>,
    commands: Commands,
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
            self.default_action_menu(),
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
                    let _ = tray.commands.send(Command::Reload);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".to_string(),
                icon_name: "application-exit".to_string(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.commands.send(Command::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

impl Tray {
    /// The "Default action" submenu: per content kind, which action to run
    /// without asking.
    ///
    /// Grouped by kind with a divider between groups, so it reads as a set of
    /// independent rules rather than one long list.
    fn default_action_menu(&self) -> ksni::MenuItem<Self> {
        use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};

        let mut entries: Vec<MenuItem<Self>> = Vec::new();

        for kind in ContentKind::all() {
            let actions = self.app.actions_for(kind);
            if actions.is_empty() {
                continue;
            }

            if !entries.is_empty() {
                entries.push(MenuItem::Separator);
            }

            // A disabled item as a group heading: the tray protocol has no
            // section label of its own.
            entries.push(
                StandardItem {
                    label: heading_for(kind),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );

            let current = self.app.default_action(kind);
            entries.push(
                CheckmarkItem {
                    label: "Ask every time".to_string(),
                    checked: current.is_none(),
                    activate: Box::new(move |tray: &mut Self| {
                        tray.app.set_default_action(kind, None);
                    }),
                    ..Default::default()
                }
                .into(),
            );

            for (id, label) in actions {
                let checked = current.as_deref() == Some(id.as_str());
                entries.push(
                    CheckmarkItem {
                        label,
                        checked,
                        activate: Box::new(move |tray: &mut Self| {
                            // Choosing the one already set turns it off again,
                            // so the submenu behaves like a set of radio
                            // buttons that can all be cleared.
                            let next = if checked { None } else { Some(id.clone()) };
                            tray.app.set_default_action(kind, next);
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }

        SubMenu {
            label: "Default action (this session)".to_string(),
            submenu: entries,
            ..Default::default()
        }
        .into()
    }
}

/// The heading shown above a kind's choices.
fn heading_for(kind: ContentKind) -> String {
    match kind {
        ContentKind::Files => "Files",
        ContentKind::Video => "Video",
        ContentKind::Image => "Images",
        ContentKind::Html => "Rich text",
        ContentKind::Url => "Links",
        ContentKind::Text => "Text",
    }
    .to_string()
}

/// Publishes the tray on a small runtime of its own.
pub fn start(app: Arc<App>, commands: Commands) -> Result<String> {
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

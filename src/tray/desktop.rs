//! The tray icon on Windows and macOS.
//!
//! > **Status:** written but not exercised — this project has so far only been
//! > run on Linux. The Linux path is the verified one.
//!
//! On macOS the tray must be created on the main thread, which this application
//! gives to the UI event loop. That is an open problem rather than something
//! this module papers over: the failure is reported so the app continues
//! without a tray icon rather than appearing to work.

use crate::app::App;
use crate::command::{Command, Commands};
use anyhow::{Context, Result};
use std::sync::Arc;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::TrayIconBuilder;

pub fn start(app: Arc<App>, commands: Commands) -> Result<String> {
    let menu = Menu::new();

    let auto = CheckMenuItem::new("Auto-shorten copied links", true, app.auto_shorten(), None);
    let edit = MenuItem::new("Edit configuration…", true, None);
    let reload = MenuItem::new("Reload configuration", true, None);
    let quit = MenuItem::new("Quit", true, None);

    menu.append(&auto).context("building the tray menu")?;
    menu.append(&PredefinedMenuItem::separator())
        .context("building the tray menu")?;
    menu.append(&edit).context("building the tray menu")?;
    menu.append(&reload).context("building the tray menu")?;
    menu.append(&PredefinedMenuItem::separator())
        .context("building the tray menu")?;
    menu.append(&quit).context("building the tray menu")?;

    let (auto_id, edit_id, reload_id, quit_id) = (
        auto.id().clone(),
        edit.id().clone(),
        reload.id().clone(),
        quit.id().clone(),
    );

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Clip Convert")
        .build()
        .context("publishing the tray icon")?;

    std::thread::spawn(move || {
        // The icon must outlive the thread that services its menu.
        let _tray = tray;
        let receiver = MenuEvent::receiver();

        while let Ok(event) = receiver.recv() {
            if event.id == auto_id {
                let now_on = !app.auto_shorten();
                app.set_auto_shorten(now_on);
                auto.set_checked(now_on);
            } else if event.id == edit_id {
                open_config(app.config_path());
            } else if event.id == reload_id {
                let _ = commands.send(Command::Reload);
            } else if event.id == quit_id {
                let _ = commands.send(Command::Quit);
                break;
            }
        }
    });

    Ok("tray-icon".to_string())
}

/// Opens the config file in whatever the system considers its editor.
fn open_config(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let command = ("cmd", vec!["/C", "start", ""]);
    #[cfg(target_os = "macos")]
    let command = ("open", Vec::new());

    let (program, args) = command;
    match std::process::Command::new(program)
        .args(args)
        .arg(path)
        .spawn()
    {
        Ok(_) => log::debug!("opened {}", path.display()),
        Err(e) => log::warn!("could not open {}: {e}", path.display()),
    }
}

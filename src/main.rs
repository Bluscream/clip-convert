//! clip-convert: a tray app that acts on whatever is in the clipboard.
//!
//! Two things happen in the background. Copied URLs are shortened automatically
//! (toggleable from the tray), and a global hotkey opens a menu of actions for
//! the current clipboard content. Which actions exist, what they are called and
//! what they do is entirely config-driven; see `config.toml`.
//!
//! The design is event-driven throughout: the clipboard is watched through a
//! change subscription where the platform offers one, the keyboard through
//! blocking reads, and the dialog window stays hidden and unpainted until it is
//! needed. With nothing happening, the process is not scheduled at all.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod hotkey;
mod instance;
mod tray;
mod ui;

use anyhow::{Context, Result};
use app::App;
use clipconv::clipboard;
use hotkey::Modifiers;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, OnceLock};
use ui::{DialogWindow, Ui, UiCommand};

fn main() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("clip_convert=info,clipconv=info"),
    )
    .init();

    // Held for the whole run; released by the kernel however the process exits.
    let _instance = instance::acquire()?;

    let modifiers = Arc::new(Modifiers::default());
    let config_path = clipconv::config::config_path();
    log::info!("configuration: {}", config_path.display());

    let app = Arc::new(App::load(config_path, Arc::clone(&modifiers))?);

    let (dialogs_tx, dialogs_rx) = mpsc::channel();
    let (commands_tx, commands_rx) = mpsc::channel::<UiCommand>();
    // Filled in by the UI thread on its first frame, so workers can wake the
    // window while it is hidden and idle.
    let context = Arc::new(OnceLock::new());
    let user_interface = Ui::new(dialogs_tx, Arc::clone(&context));

    install_signal_handler(commands_tx.clone())?;

    match tray::start(Arc::clone(&app), commands_tx.clone()) {
        Ok(backend) => log::info!("tray: {backend}"),
        // Degraded, not fatal: the hotkey and auto-shortening still work.
        Err(e) => log::warn!("continuing without a tray icon: {e:#}"),
    }

    start_clipboard_watch(Arc::clone(&app));
    start_hotkey(&app, &modifiers, user_interface);

    let reloader = {
        let app = Arc::clone(&app);
        Box::new(move || app.reload().map_err(|e| format!("{e:#}")))
    };

    log::info!("ready");
    run_window(dialogs_rx, commands_rx, context, reloader)
}

/// Runs the dialog window, which owns the main thread.
///
/// The window starts hidden: this is a tray application, and the first thing a
/// user should see is nothing at all.
fn run_window(
    dialogs: mpsc::Receiver<ui::dialog::Active>,
    commands: mpsc::Receiver<UiCommand>,
    context: Arc<OnceLock<egui::Context>>,
    reloader: Box<dyn Fn() -> Result<(), String>>,
) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Clip Convert")
            .with_inner_size([460.0, 320.0])
            .with_visible(false)
            .with_resizable(false)
            .with_always_on_top(),
        ..Default::default()
    };

    eframe::run_native(
        "Clip Convert",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(DialogWindow::new(
                dialogs, commands, context, reloader,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("the dialog window could not start: {e}"))
}

/// Quits cleanly on SIGINT and SIGTERM.
///
/// Without this the process dies before the clipboard watcher's destructor runs,
/// which on Wayland leaves its helper orphaned and still connected to the
/// compositor.
#[cfg(unix)]
fn install_signal_handler(commands: Sender<UiCommand>) -> Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
    ])
    .context("installing the signal handler")?;

    std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            log::info!("received signal {signal}; shutting down");
            let _ = commands.send(UiCommand::Quit);
        }
    });
    Ok(())
}

#[cfg(not(unix))]
fn install_signal_handler(_commands: Sender<UiCommand>) -> Result<()> {
    Ok(())
}

/// Watches the clipboard and runs the auto-shorten pipeline.
fn start_clipboard_watch(app: Arc<App>) {
    std::thread::spawn(move || {
        let watcher = match clipboard::Watcher::start() {
            Ok(watcher) => watcher,
            Err(e) => {
                log::error!("clipboard watching is unavailable: {e}");
                return;
            }
        };

        // Held for the lifetime of the thread; dropping it stops the watcher.
        while watcher.changes().recv().is_ok() {
            app.on_clipboard_change();
        }
        log::warn!("clipboard watcher stopped");
    });
}

/// Listens for the configured hotkey and runs the action pipeline.
fn start_hotkey(app: &Arc<App>, modifiers: &Arc<Modifiers>, user_interface: Ui) {
    let spec = app.hotkey();
    let chord = match hotkey::parse_chord(&spec) {
        Ok(chord) => chord,
        Err(e) => {
            log::error!("{e:#}");
            app::notify("Hotkey not set up", &format!("{e:#}"));
            return;
        }
    };

    let (tx, rx) = mpsc::channel();
    match hotkey::listen(&chord, modifiers, &tx) {
        Ok(backend) => log::info!("listening for {spec} via {backend}"),
        Err(e) => {
            log::error!("{e:#}");
            app::notify("Hotkey unavailable", &format!("{e:#}"));
            return;
        }
    }

    let app = Arc::clone(app);
    std::thread::spawn(move || {
        // Handled one at a time on this thread, so two quick presses cannot
        // open two menus.
        while rx.recv().is_ok() {
            app.on_hotkey(&user_interface);
        }
    });
}

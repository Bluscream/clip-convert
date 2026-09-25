//! clip-convert: a tray app that acts on whatever is in the clipboard.
//!
//! Two things happen in the background. Copied URLs are shortened automatically
//! (toggleable from the tray), and a global hotkey opens a menu of actions for
//! the current clipboard content. Which actions exist, what they are called and
//! what they do is entirely config-driven; see `config.toml`.
//!
//! This process holds no graphical toolkit. Dialogs are drawn by
//! `clip-convert-dialog`, which is started when one is needed and exits when it
//! closes — so nothing is on screen, and nothing is held on the graphics driver,
//! while the app sits in the tray.
//!
//! Everything else is event-driven too: the clipboard is watched through a
//! change subscription where the platform offers one, and the keyboard through
//! blocking reads. With nothing happening, the process is not scheduled at all.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod command;
mod hotkey;
mod instance;
mod prompt;
mod tray;

use anyhow::{Context, Result};
use app::App;
use clipconv::clipboard;
use command::{Command, Commands};
use hotkey::Modifiers;
use prompt::Prompter;
use std::sync::mpsc;
use std::sync::Arc;

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
    let prompter = Prompter::new(Arc::clone(&app) as Arc<dyn prompt::Store>)?;

    let (commands_tx, commands_rx) = mpsc::channel();
    install_signal_handler(commands_tx.clone())?;

    match tray::start(Arc::clone(&app), commands_tx.clone()) {
        Ok(backend) => log::info!("tray: {backend}"),
        // Degraded, not fatal: the hotkey and auto-shortening still work.
        Err(e) => log::warn!("continuing without a tray icon: {e:#}"),
    }

    // Kept in scope: dropping it at the end of main kills the watcher child.
    // Owned by the consuming thread, its destructor would never run.
    let _watcher = start_clipboard_watch(Arc::clone(&app));
    start_hotkey(&app, &modifiers, prompter.clone());

    log::info!("ready");
    serve(&app, &prompter, &commands_rx);
    Ok(())
}

/// The main thread: blocked on the command channel until something happens.
///
/// No polling and no event loop — the thread is simply not scheduled until the
/// tray or a signal posts something.
fn serve(app: &Arc<App>, prompter: &Prompter, commands: &mpsc::Receiver<Command>) {
    while let Ok(command) = commands.recv() {
        match command {
            Command::Reload => match app.reload() {
                Ok(()) => app::notify("Configuration reloaded", "The new settings are active."),
                Err(e) => {
                    log::error!("reload failed: {e:#}");
                    prompter.error(&format!("Could not reload the configuration:\n\n{e:#}"));
                }
            },
            Command::Quit => {
                log::info!("quitting");
                return;
            }
        }
    }
}

/// Quits cleanly on SIGINT and SIGTERM.
///
/// Without this the process dies before the clipboard watcher's destructor runs,
/// which on Wayland leaves its helper orphaned and still connected to the
/// compositor.
#[cfg(unix)]
fn install_signal_handler(commands: Commands) -> Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
    ])
    .context("installing the signal handler")?;

    std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            log::info!("received signal {signal}; shutting down");
            let _ = commands.send(Command::Quit);
        }
    });
    Ok(())
}

#[cfg(not(unix))]
fn install_signal_handler(_commands: Commands) -> Result<()> {
    Ok(())
}

/// Watches the clipboard and runs the auto-shorten pipeline.
///
/// Returns the watcher, which the caller must keep alive.
fn start_clipboard_watch(app: Arc<App>) -> Option<clipboard::Watcher> {
    let (watcher, changes) = match clipboard::Watcher::start() {
        Ok(started) => started,
        Err(e) => {
            log::error!("clipboard watching is unavailable: {e}");
            return None;
        }
    };
    log::debug!("clipboard backend: {}", watcher.backend());

    std::thread::spawn(move || {
        while changes.recv().is_ok() {
            app.on_clipboard_change();
        }
        log::debug!("clipboard watcher stopped");
    });

    Some(watcher)
}

/// Listens for the configured hotkey and runs the action pipeline.
fn start_hotkey(app: &Arc<App>, modifiers: &Arc<Modifiers>, prompter: Prompter) {
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
            app.on_hotkey(&prompter);
        }
    });
}

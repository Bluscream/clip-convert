//! linux-clip-convert: a tray app that acts on clipboard content.
//!
//! Two things happen in the background. Copied URLs are shortened automatically
//! (toggleable from the tray), and a global hotkey opens a menu of actions for
//! whatever is on the clipboard. Which actions exist, what they are called and
//! what they do is entirely config-driven; see `config.toml`.
//!
//! Everything is event-driven. The clipboard is watched through
//! `wl-paste --watch` and the keyboard through blocking evdev reads, so with
//! nothing happening the process is not scheduled at all.

mod app;
mod dialog;
mod hotkey;
mod instance;
mod theme;
mod tray;
mod ui;

use anyhow::{Context, Result};
use app::App;
use hotkey::Modifiers;
use lcc_core::clipboard;
use std::sync::mpsc;
use std::sync::Arc;
use tray::TrayCommand;
use ui::{Ui, UiRequest};

fn main() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("linux_clip_convert=info,lcc_core=info"),
    )
    .init();

    // Held for the whole run; released by the kernel however the process exits.
    let _instance = instance::acquire()?;

    gtk::init().map_err(|e| anyhow::anyhow!("could not initialise GTK: {e}"))?;

    dialog::apply_color_scheme(theme::prefers_dark(dialog::gtk_prefers_dark()));
    dialog::install_css();

    let modifiers = Arc::new(Modifiers::default());
    let config_path = lcc_core::config::config_path();
    log::info!("configuration: {}", config_path.display());

    let app = Arc::new(App::load(config_path, Arc::clone(&modifiers))?);

    // Dialog requests and tray commands both have to end up on the GTK thread.
    let (ui_tx, ui_rx) = async_channel::unbounded::<UiRequest>();
    let (tray_tx, tray_rx) = mpsc::channel::<TrayCommand>();
    let (main_tx, main_rx) = async_channel::unbounded::<TrayCommand>();

    let context = glib::MainContext::default();
    context.spawn_local(async move {
        while let Ok(request) = ui_rx.recv().await {
            request.serve();
        }
    });

    {
        let app = Arc::clone(&app);
        context.spawn_local(async move {
            while let Ok(command) = main_rx.recv().await {
                match command {
                    TrayCommand::Reload => match app.reload() {
                        Ok(()) => {
                            app::notify("Configuration reloaded", "The new settings are active.");
                        }
                        Err(e) => {
                            log::error!("reload failed: {e:#}");
                            dialog::show_error(&format!(
                                "Could not reload the configuration:\n\n{e:#}"
                            ));
                        }
                    },
                    TrayCommand::Quit => {
                        log::info!("quitting");
                        gtk::main_quit();
                        return;
                    }
                }
            }
        });
    }

    // The tray callbacks run on ksni's own thread; forward their commands to GTK.
    std::thread::spawn(move || {
        while let Ok(command) = tray_rx.recv() {
            if main_tx.send_blocking(command).is_err() {
                break;
            }
        }
    });

    install_signal_handler(tray_tx.clone())?;
    start_tray(Arc::clone(&app), tray_tx)?;
    start_clipboard_watch(Arc::clone(&app));
    start_hotkey(&app, &modifiers, Ui::new(ui_tx));

    log::info!("ready");
    gtk::main();
    Ok(())
}

/// Quits cleanly on SIGINT and SIGTERM.
///
/// Without this the process dies before `Watcher`'s destructor runs, leaving the
/// `wl-paste --watch` child orphaned and still connected to the compositor.
fn install_signal_handler(commands: mpsc::Sender<TrayCommand>) -> Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
    ])
    .context("installing the signal handler")?;

    std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            log::info!("received signal {signal}; shutting down");
            let _ = commands.send(TrayCommand::Quit);
        }
    });
    Ok(())
}

/// Publishes the tray icon on a tokio runtime of its own.
fn start_tray(app: Arc<App>, commands: mpsc::Sender<TrayCommand>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the tray runtime")?;

    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::spawn(move || {
        runtime.block_on(async move {
            use ksni::TrayMethods;
            let result = tray::Tray::new(app, commands).spawn().await;
            match result {
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

    match ready_rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Ok(())) => {
            log::info!("tray icon published");
            Ok(())
        }
        Ok(Err(e)) => anyhow::bail!("could not publish the tray icon: {e}"),
        Err(_) => {
            // No StatusNotifierWatcher is a degraded state, not a fatal one: the
            // hotkey and auto-shortening still work without a tray icon.
            log::warn!("no tray host responded; continuing without a tray icon");
            Ok(())
        }
    }
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
        log::info!("watching the clipboard");

        // Held for the lifetime of the thread; dropping it kills the watcher.
        while watcher.changes().recv().is_ok() {
            app.on_clipboard_change();
        }
        log::warn!("clipboard watcher stopped");
    });
}

/// Listens for the configured hotkey and runs the action pipeline.
fn start_hotkey(app: &Arc<App>, modifiers: &Arc<Modifiers>, ui: Ui) {
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
        Ok(count) => log::info!("listening for {spec} on {count} keyboard(s)"),
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
            app.on_hotkey(&ui);
        }
    });
}

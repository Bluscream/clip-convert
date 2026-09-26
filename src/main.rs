//! clip-convert: a tray app that acts on whatever is in the clipboard.
//!
//! A global hotkey opens a menu of actions for whatever is on the clipboard.
//! Which actions exist, what they are called and what they do is entirely
//! config-driven; see `config.toml`.
//!
//! This process holds no graphical toolkit. Dialogs are drawn by
//! `clip-convert-dialog`, which is started when one is needed and exits when it
//! closes — so nothing is on screen, and nothing is held on the graphics driver,
//! while the app sits in the tray.
//!
//! Everything is event-driven: the keyboard is read through blocking reads,
//! and the clipboard only when the hotkey is pressed. With nothing happening,
//! the process is not scheduled at all.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;
mod command;
mod hotkey;
mod instance;
mod prompt;
mod tray;

use anyhow::{Context, Result};
use app::App;
use command::{Command, Commands};
use hotkey::Modifiers;
use prompt::Prompter;
use std::sync::mpsc;
use std::sync::Arc;

/// Makes a panic anywhere kill the whole process, loudly.
///
/// This used to be `panic = "abort"` in the release profile, which one
/// dependency cannot be built with. The behaviour is what matters and it is
/// kept here: a panic in a worker thread would otherwise take only that thread
/// with it, leaving a tray icon whose hotkey or clipboard watcher silently no
/// longer works — the worst of both outcomes.
fn abort_on_panic() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        log::error!("panicked: {info}");
        std::process::abort();
    }));
}

/// What the command line asked for.
///
/// There is almost nothing to parse: this is a daemon, and every setting lives
/// in the config file. `--help` and `--version` are here because a binary that
/// cannot say what it is leaves someone who downloaded it with no way to find
/// out — and both must work while another copy is already running, so they are
/// answered before the single-instance lock is taken.
enum Invocation {
    Run,
    Help,
    Version,
    Unknown(String),
}

fn parse_arguments(arguments: &[String]) -> Invocation {
    match arguments.first().map(String::as_str) {
        None => Invocation::Run,
        Some("--help" | "-h") => Invocation::Help,
        Some("--version" | "-V") => Invocation::Version,
        Some(other) => Invocation::Unknown(other.to_string()),
    }
}

const USAGE: &str = "\
clip-convert — act on whatever is in the clipboard

Usage:
  clip-convert            run the tray and listen for the hotkey
  clip-convert --help     show this
  clip-convert --version  show the version

Everything else is configured in the config file, whose path is printed at
startup. The tray menu has an entry to open it.";

fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match parse_arguments(&arguments) {
        Invocation::Run => {}
        Invocation::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        Invocation::Version => {
            println!("clip-convert {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Invocation::Unknown(argument) => {
            eprintln!("clip-convert: unknown argument `{argument}`\n");
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }

    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("clip_convert=info,clipconv=info"),
    )
    .init();
    abort_on_panic();

    // Held for the whole run; released by the kernel however the process exits.
    let _instance = instance::acquire()?;

    let modifiers = Arc::new(Modifiers::default());
    let config_path = clipconv::config::config_path();
    log::info!("configuration: {}", config_path.display());

    let app = Arc::new(App::load(config_path)?);
    let prompter = Prompter::new(Arc::clone(&app) as Arc<dyn prompt::Store>)?;

    let (commands_tx, commands_rx) = mpsc::channel();
    install_signal_handler(commands_tx.clone())?;

    match tray::start(Arc::clone(&app), commands_tx.clone()) {
        Ok(backend) => log::info!("tray: {backend}"),
        // Degraded, not fatal: the hotkey still works.
        Err(e) => log::warn!("continuing without a tray icon: {e:#}"),
    }

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

/// Quits cleanly on SIGINT and SIGTERM, so the tray is withdrawn and any
/// helper process started for a dialog is not left behind.
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

#[cfg(test)]
mod tests {
    use super::{parse_arguments, Invocation};

    fn parse(arguments: &[&str]) -> Invocation {
        let owned: Vec<String> = arguments.iter().map(|a| (*a).to_string()).collect();
        parse_arguments(&owned)
    }

    #[test]
    fn no_arguments_runs_the_tray() {
        assert!(matches!(parse(&[]), Invocation::Run));
    }

    #[test]
    fn help_and_version_are_recognised_both_ways() {
        assert!(matches!(parse(&["--help"]), Invocation::Help));
        assert!(matches!(parse(&["-h"]), Invocation::Help));
        assert!(matches!(parse(&["--version"]), Invocation::Version));
        assert!(matches!(parse(&["-V"]), Invocation::Version));
    }

    #[test]
    fn an_unknown_argument_is_named_rather_than_ignored() {
        // Silently starting the tray anyway is what this replaces: a typo in a
        // desktop entry or a service file would otherwise look like it worked.
        match parse(&["--tray"]) {
            Invocation::Unknown(argument) => assert_eq!(argument, "--tray"),
            _ => panic!("expected Unknown"),
        }
    }
}

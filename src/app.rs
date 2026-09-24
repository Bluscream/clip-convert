//! Application state and the two pipelines that drive it.
//!
//! Both pipelines run off the GTK thread. The hotkey pipeline reads the
//! clipboard, offers the menu and carries out the chosen action; the clipboard
//! pipeline decides whether a newly copied URL should be shortened.

use crate::hotkey::Modifiers;
use crate::prompt::Prompter;
use anyhow::{Context, Result};
use clipconv::action::Action;
use clipconv::config::{self, Config};
use clipconv::content::Clip;
use clipconv::{auto, clipboard, runner, shorten, typing};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Everything the app knows, shared between threads.
pub struct App {
    config: Mutex<Config>,
    actions: Mutex<Vec<Action>>,
    config_path: PathBuf,
    /// The last value this app wrote to the clipboard, so it never reacts to
    /// its own output.
    last_written: Mutex<Option<String>>,
    /// The URL most recently considered, for the double-copy rule.
    last_seen: Mutex<Option<String>>,
    /// Set while an action pipeline is running, so a second hotkey press cannot
    /// open a second menu on top of the first.
    busy: AtomicBool,
    modifiers: Arc<Modifiers>,
}

impl App {
    /// Loads the config and validates it.
    ///
    /// # Errors
    ///
    /// Returns an error if the config file cannot be read or is invalid.
    pub fn load(config_path: PathBuf, modifiers: Arc<Modifiers>) -> Result<Self> {
        let config = config::load_from(&config_path)
            .with_context(|| format!("loading {}", config_path.display()))?;
        let actions = config.validate().context("validating the configuration")?;

        Ok(Self {
            config: Mutex::new(config),
            actions: Mutex::new(actions),
            config_path,
            last_written: Mutex::new(None),
            last_seen: Mutex::new(None),
            busy: AtomicBool::new(false),
            modifiers,
        })
    }

    /// A snapshot of the config, so no lock is held while work is done.
    fn config(&self) -> Config {
        // A poisoned lock means another thread panicked mid-update. The config
        // is still readable and losing the tray would be worse, so recover it.
        self.config
            .lock()
            .map_or_else(|poisoned| poisoned.into_inner().clone(), |c| c.clone())
    }

    fn actions(&self) -> Vec<Action> {
        self.actions
            .lock()
            .map_or_else(|poisoned| poisoned.into_inner().clone(), |a| a.clone())
    }

    /// Whether auto-shortening is currently on.
    #[must_use]
    pub fn auto_shorten(&self) -> bool {
        self.config().auto_shorten
    }

    /// The configured hotkey spec, e.g. `ctrl+b`.
    #[must_use]
    pub fn hotkey(&self) -> String {
        self.config().hotkey
    }

    /// The path the config is read from, for the tray's "Edit config" entry.
    #[must_use]
    pub fn config_path(&self) -> &std::path::Path {
        &self.config_path
    }

    /// Re-reads the config from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the file is unreadable or invalid, leaving the
    /// running config untouched so a bad edit cannot half-apply.
    pub fn reload(&self) -> Result<()> {
        let config = config::load_from(&self.config_path)
            .with_context(|| format!("loading {}", self.config_path.display()))?;
        let actions = config.validate().context("validating the configuration")?;

        if let Ok(mut guard) = self.config.lock() {
            *guard = config;
        }
        if let Ok(mut guard) = self.actions.lock() {
            *guard = actions;
        }
        log::info!("configuration reloaded");
        Ok(())
    }

    /// Changes a setting and writes the file back.
    fn update(&self, change: impl FnOnce(&mut Config)) {
        let Ok(mut guard) = self.config.lock() else {
            log::error!("config lock is poisoned; not saving");
            return;
        };
        change(&mut guard);
        if let Err(e) = config::save_to(&self.config_path, &guard) {
            log::error!("could not save the configuration: {e}");
        }
    }

    /// Turns auto-shortening on or off and remembers the choice.
    pub fn set_auto_shorten(&self, on: bool) {
        self.update(|c| c.auto_shorten = on);
    }

    /// Remembers the action dialog's checkbox.
    fn set_paste_after(&self, on: bool) {
        self.update(|c| c.paste_after_action = on);
    }

    /// Records what the app just put on the clipboard.
    fn remember_written(&self, value: &str) {
        if let Ok(mut guard) = self.last_written.lock() {
            *guard = Some(value.to_string());
        }
    }

    /// The hotkey pipeline: read the clipboard, offer the menu, run the choice.
    pub fn on_hotkey(&self, prompter: &Prompter) {
        // `swap` rather than load-then-store: two hotkey threads can arrive at
        // once, and only one may proceed.
        if self.busy.swap(true, Ordering::SeqCst) {
            log::debug!("ignoring hotkey: an action is already running");
            return;
        }
        let result = self.run_hotkey(prompter);
        self.busy.store(false, Ordering::SeqCst);

        if let Err(e) = result {
            log::warn!("action failed: {e:#}");
            prompter.error(&format!("{e:#}"));
        }
    }

    fn run_hotkey(&self, prompter: &Prompter) -> Result<()> {
        let clip = match clipboard::read() {
            Ok(clip) => clip,
            Err(clipboard::ClipboardError::Empty) => {
                notify("Clipboard is empty", "Copy something first.");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };

        let config = self.config();
        let actions = self.actions();
        let offered = clipconv::action::for_kind(&actions, clip.kind());
        if offered.is_empty() {
            notify(
                "No actions available",
                &format!("Nothing is configured for {} content.", clip.kind()),
            );
            return Ok(());
        }

        let labelled: Vec<(String, String)> = offered
            .iter()
            .map(|a| (a.id.clone(), a.label.clone()))
            .collect();

        log::debug!(
            "offering {} actions for {} content",
            labelled.len(),
            clip.kind()
        );
        let Some(choice) =
            prompter.choose_action(&clip.describe(), &labelled, config.paste_after_action)
        else {
            log::debug!("action menu dismissed");
            return Ok(());
        };

        if choice.paste_after != config.paste_after_action {
            self.set_paste_after(choice.paste_after);
        }

        log::debug!("chose {}", choice.action_id);
        let Some(action) = offered.iter().find(|a| a.id == choice.action_id) else {
            log::warn!("chosen action {} vanished", choice.action_id);
            return Ok(());
        };

        // The dialog had keyboard focus. Typing or pasting immediately would go
        // nowhere, or into the wrong window, until the compositor has handed
        // focus back to where the user actually was.
        std::thread::sleep(Duration::from_millis(config.focus_restore_delay_ms));

        let outcome = runner::run(action, &clip, &config, prompter)?;
        let Some(outcome) = outcome else {
            log::debug!("action cancelled at a prompt");
            return Ok(());
        };

        // Remember the new clipboard value so the auto-shorten watcher, which is
        // about to see this change, recognises it as our own.
        if outcome.clipboard_changed {
            if let Ok(current) = clipboard::read() {
                if let Some(text) = current.as_text() {
                    self.remember_written(text);
                }
            }
        }

        if choice.paste_after && outcome.clipboard_changed {
            typing::press_paste(&config.commands).context("pasting after the action")?;
        }

        log::info!("{} finished: {}", action.id, outcome.message);
        notify(&action.label, &outcome.message);
        Ok(())
    }

    /// The clipboard pipeline: shorten a newly copied URL, if it qualifies.
    pub fn on_clipboard_change(&self) {
        if let Err(e) = self.try_auto_shorten() {
            log::warn!("auto-shorten failed: {e:#}");
        }
    }

    fn try_auto_shorten(&self) -> Result<()> {
        let clip = match clipboard::read() {
            Ok(clip) => clip,
            Err(clipboard::ClipboardError::Empty) => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        let Clip::Url(url) = clip else {
            return Ok(());
        };

        let config = self.config();
        let shorteners = config.active_shorteners();

        let last_written = self.last_written.lock().ok().and_then(|g| g.clone());
        let last_seen = self.last_seen.lock().ok().and_then(|g| g.clone());
        let context = auto::Context {
            last_written: last_written.as_deref(),
            last_seen: last_seen.as_deref(),
            bypass_held: self.modifiers.shift_held(),
            scroll_lock: crate::hotkey::scroll_lock_on(),
        };

        if let Err(skip) = auto::decide(&url, &config, &shorteners, context) {
            log::debug!("not shortening {url}: {}", skip.reason());
            return Ok(());
        }

        if let Ok(mut guard) = self.last_seen.lock() {
            *guard = Some(url.as_str().to_string());
        }

        let chosen = shorten::pick(&shorteners).context("no shortener available")?;
        let short = shorten::shorten(&url, chosen, config.ignore_ssl_errors)
            .with_context(|| format!("shortening via {}", chosen.name))?;

        // Recorded before the write, so the clipboard change this causes is
        // already recognisable as our own by the time it arrives.
        self.remember_written(short.as_str());
        clipboard::write_text(short.as_str()).context("writing the short URL back")?;

        if config.notifications {
            notify("Link shortened", &format!("{url}\n→ {short}"));
        }
        Ok(())
    }
}

/// Shows a desktop notification, logging rather than failing if it cannot.
pub fn notify(summary: &str, body: &str) {
    if let Err(e) = notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .appname("clip-convert")
        .show()
    {
        log::debug!("could not show a notification: {e}");
    }
}

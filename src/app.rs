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
use clipconv::content::{Clip, ContentKind};
use clipconv::{auto, clipboard, runner, shorten, typing};
use std::collections::BTreeMap;
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
    /// Per content kind, the action to run without asking.
    ///
    /// Deliberately not persisted: this is a "for now" setting, so restarting
    /// the app brings the menu back rather than leaving a rule someone set
    /// weeks ago and forgot.
    defaults: Mutex<BTreeMap<ContentKind, String>>,
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
        let mut config = config::load_from(&config_path)
            .with_context(|| format!("loading {}", config_path.display()))?;

        // Icons written as a URL or a path are fetched once and stored back as
        // base64, so later runs never need the original.
        if config.resolve_icons() {
            if let Err(e) = config::save_to(&config_path, &config) {
                log::warn!("could not cache resolved icons: {e}");
            }
        }

        let actions = config.validate().context("validating the configuration")?;

        Ok(Self {
            config: Mutex::new(config),
            actions: Mutex::new(actions),
            config_path,
            last_written: Mutex::new(None),
            last_seen: Mutex::new(None),
            defaults: Mutex::new(BTreeMap::new()),
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

    /// The action that runs without asking for `kind`, if one is set.
    #[must_use]
    pub fn default_action(&self, kind: ContentKind) -> Option<String> {
        self.defaults
            .lock()
            .ok()
            .and_then(|defaults| defaults.get(&kind).cloned())
    }

    /// Sets, or with `None` clears, the action that runs without asking.
    pub fn set_default_action(&self, kind: ContentKind, action: Option<String>) {
        let Ok(mut defaults) = self.defaults.lock() else {
            return;
        };
        match action {
            Some(id) => {
                log::info!("{kind} will run `{id}` without asking");
                defaults.insert(kind, id);
            }
            None => {
                defaults.remove(&kind);
            }
        }
    }

    /// The actions that could be offered for `kind`, as `(id, label)`.
    ///
    /// Uses the configured labels, since the tray has no clipboard content to
    /// name a subject from.
    #[must_use]
    pub fn actions_for(&self, kind: ContentKind) -> Vec<(String, String)> {
        let available = std::collections::BTreeSet::from([kind]);
        clipconv::action::for_kinds(&self.actions(), &available)
            .iter()
            .map(|a| (a.id.clone(), a.label.clone()))
            .collect()
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
        let kinds = clip.kinds();
        let offered = clipconv::action::for_kinds(&actions, &kinds);
        if offered.is_empty() {
            notify(
                "No actions available",
                &format!(
                    "Nothing is configured for {} content.",
                    kinds
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" or ")
                ),
            );
            return Ok(());
        }

        let Some(chosen_id) = self.choose(&clip, &offered, &config, prompter) else {
            log::debug!("action menu dismissed");
            return Ok(());
        };
        let paste_after = self.config().paste_after_action;

        log::debug!("chose {chosen_id}");
        let Some(action) = offered.iter().find(|a| a.id == chosen_id) else {
            log::warn!("chosen action {chosen_id} vanished");
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
                if let Some(text) = current.text() {
                    self.remember_written(text);
                }
            }
        }

        if paste_after && outcome.clipboard_changed {
            typing::press_paste(&config.commands).context("pasting after the action")?;
        }

        log::info!("{} finished: {}", action.id, outcome.message);
        notify(&action.label, &outcome.message);
        Ok(())
    }

    /// Works out which action to run: the session default for this content's
    /// source kind, or whatever the user picks from the menu.
    ///
    /// Returns `None` when the menu was dismissed.
    fn choose(
        &self,
        clip: &Clip,
        offered: &[&Action],
        config: &Config,
        prompter: &Prompter,
    ) -> Option<String> {
        let labelled: Vec<clipconv::protocol::ActionEntry> = offered
            .iter()
            .map(|a| clipconv::protocol::ActionEntry {
                id: a.id.clone(),
                label: a.display_label(clip),
                icon: a.icon.clone(),
                button_color: a.button_color.clone(),
                text_color: a.text_color.clone(),
            })
            .collect();

        // A default for this kind skips the menu entirely. It is ignored when
        // the action it names is not among those offered, which happens if the
        // config changed since it was set.
        let source = clip.source_kind();
        if let Some(id) = self.default_action(source) {
            if labelled.iter().any(|entry| entry.id == id) {
                log::debug!("running `{id}` without asking: it is the default for {source}");
                return Some(id);
            }
        }

        log::debug!("offering {} actions for this clipboard", labelled.len());
        let choice =
            prompter.choose_action(&clip.describe(), &labelled, config.paste_after_action)?;
        if choice.paste_after != config.paste_after_action {
            self.set_paste_after(choice.paste_after);
        }
        Some(choice.action_id)
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

        // Only a clipboard that is purely a URL is auto-shortened. A copied
        // image that merely carries a source URL is not something the user
        // asked to have rewritten.
        let Some(url) = clip.url().cloned() else {
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

impl crate::prompt::SizeStore for App {
    fn size(&self, key: &str) -> Option<clipconv::protocol::WindowSize> {
        self.config().window_sizes.get(key).copied()
    }

    fn set_size(&self, key: &str, size: clipconv::protocol::WindowSize) {
        self.update(|config| {
            config.window_sizes.insert(key.to_string(), size);
        });
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

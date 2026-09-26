//! Application state and the pipeline that drives it.
//!
//! The hotkey pipeline reads the clipboard, offers the menu and carries out
//! whichever action was chosen. It runs off the tray's thread, so a long
//! action cannot wedge the tray.

use crate::prompt::Prompter;
use anyhow::{Context, Result};
use clipconv::action::Action;
use clipconv::config::{self, Config};
use clipconv::content::{Clip, ContentKind};
use clipconv::{clipboard, runner, typing};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Everything the app knows, shared between threads.
pub struct App {
    config: Mutex<Config>,
    actions: Mutex<Vec<Action>>,
    config_path: PathBuf,
    /// Per content kind, the action to run without asking.
    ///
    /// Deliberately not persisted: this is a "for now" setting, so restarting
    /// the app brings the menu back rather than leaving a rule someone set
    /// weeks ago and forgot.
    defaults: Mutex<BTreeMap<ContentKind, String>>,
    /// Set while an action pipeline is running, so a second hotkey press cannot
    /// open a second menu on top of the first.
    busy: AtomicBool,
}

impl App {
    /// Loads the config and validates it.
    ///
    /// # Errors
    ///
    /// Returns an error if the config file cannot be read or is invalid.
    pub fn load(config_path: PathBuf) -> Result<Self> {
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
            defaults: Mutex::new(BTreeMap::new()),
            busy: AtomicBool::new(false),
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

    /// Remembers the action dialog's checkbox.
    fn set_paste_after(&self, on: bool) {
        self.update(|c| c.paste_after_action = on);
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
        let offered = clipconv::action::for_clip(&actions, &clip, &config);
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

        if paste_after && outcome.clipboard_changed {
            typing::press_paste(&config.commands, &config.terminal_classes)
                .context("pasting after the action")?;
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
}

impl crate::prompt::Store for App {
    fn size(&self, key: &str) -> Option<clipconv::protocol::WindowSize> {
        self.config().window_sizes.get(key).copied()
    }

    fn set_size(&self, key: &str, size: clipconv::protocol::WindowSize) {
        self.update(|config| {
            config.window_sizes.insert(key.to_string(), size);
        });
    }

    fn image_options(&self) -> clipconv::cutout::ImageOptions {
        self.config().image_options
    }

    fn set_image_options(&self, options: clipconv::cutout::ImageOptions) {
        self.update(|config| config.image_options = options);
    }

    fn remember_replace(&self, replacement: &clipconv::replace::Replacement) {
        self.update(|config| {
            let limit = config.replace.history_limit;
            clipconv::replace::remember(&mut config.replace.patterns, &replacement.pattern, limit);
            clipconv::replace::remember(
                &mut config.replace.replacements,
                &replacement.replacement,
                limit,
            );
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

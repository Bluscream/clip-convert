//! The dialog window, and the handle workers use to drive it.
//!
//! Actions run on worker threads, because typing, converting and network calls
//! all take long enough that doing them on the UI thread would freeze the
//! dialogs. egui may only be touched from its own thread, so a worker that needs
//! an answer sends an [`dialog::Active`] carrying a reply channel and blocks on
//! it; the UI thread shows the dialog and sends the answer back.
//!
//! One window is reused and hidden between dialogs rather than created each
//! time, so nothing is allocated on the graphics driver while the app idles in
//! the tray.

pub mod dialog;

use clipconv::presets::Preset;
use clipconv::runner::Prompt;
use dialog::{ActionChoice, Active, Outcome};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};

/// A worker's handle for asking the UI thread things.
#[derive(Clone)]
pub struct Ui {
    requests: Sender<Active>,
    /// Set once the window exists. Used to wake it when it is idle and hidden.
    context: Arc<OnceLock<egui::Context>>,
}

impl Ui {
    #[must_use]
    pub fn new(requests: Sender<Active>, context: Arc<OnceLock<egui::Context>>) -> Self {
        Self { requests, context }
    }

    /// Sends a dialog and waits for the answer.
    ///
    /// Returns `None` if the UI thread has gone away, which only happens during
    /// shutdown; treating that as "cancelled" is the right behaviour.
    fn ask<T>(&self, build: impl FnOnce(Sender<Option<T>>) -> Active) -> Option<T> {
        let (reply, answer) = mpsc::channel();
        if self.requests.send(build(reply)).is_err() {
            log::debug!("UI channel closed; treating the prompt as cancelled");
            return None;
        }

        // The window is hidden and egui is not repainting; without this the
        // request would sit unread until some unrelated event woke it.
        if let Some(context) = self.context.get() {
            context.request_repaint();
        }

        answer.recv().ok().flatten()
    }

    /// Shows the action menu for the current clipboard content.
    pub fn choose_action(
        &self,
        description: &str,
        actions: &[(String, String)],
        paste_after: bool,
    ) -> Option<ActionChoice> {
        self.ask(|reply| Active::ChooseAction {
            description: description.to_string(),
            actions: actions.to_vec(),
            paste_after,
            reply,
        })
    }

    /// Reports a failure to the user.
    pub fn error(&self, message: &str) {
        let _ = self.requests.send(Active::ShowError {
            message: message.to_string(),
        });
        if let Some(context) = self.context.get() {
            context.request_repaint();
        }
    }
}

impl Prompt for Ui {
    fn ask_limit(&self, title: &str, message: &str, default: usize) -> Option<usize> {
        self.ask(|reply| Active::AskLimit {
            title: title.to_string(),
            message: message.to_string(),
            value: default.to_string(),
            reply,
        })
    }

    fn ask_resize_target(&self, presets: &[Preset]) -> Option<Preset> {
        self.ask(|reply| Active::AskResizeTarget {
            presets: presets.to_vec(),
            custom: None,
            reply,
        })
    }
}

/// Something the UI thread must do besides showing a dialog.
pub enum UiCommand {
    /// Re-read the config, reporting the result.
    Reload,
    /// Shut the application down.
    Quit,
}

/// The egui application: a single window that is hidden unless a dialog is up.
pub struct DialogWindow {
    incoming: Receiver<Active>,
    commands: Receiver<UiCommand>,
    active: Option<Active>,
    context: Arc<OnceLock<egui::Context>>,
    on_reload: Box<dyn Fn() -> Result<(), String>>,
    visible: bool,
}

impl DialogWindow {
    #[must_use]
    pub fn new(
        incoming: Receiver<Active>,
        commands: Receiver<UiCommand>,
        context: Arc<OnceLock<egui::Context>>,
        on_reload: Box<dyn Fn() -> Result<(), String>>,
    ) -> Self {
        Self {
            incoming,
            commands,
            active: None,
            context,
            on_reload,
            visible: false,
        }
    }

    /// Brings the window up for `next`, sized and titled for it.
    fn present(&mut self, next: Active, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(next.title().to_string()));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(dialog::window_size(&next)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        // Wayland will not always honour this; a compositor may decide an
        // application cannot focus itself. The dialog is still usable by click.
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.visible = true;
        self.active = Some(next);
    }

    /// Puts the window away again.
    fn dismiss(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.visible = false;
    }
}

impl eframe::App for DialogWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Published on the first frame so workers can wake the window later.
        let _ = self.context.set(ctx.clone());

        while let Ok(command) = self.commands.try_recv() {
            match command {
                UiCommand::Reload => match (self.on_reload)() {
                    Ok(()) => {
                        crate::app::notify("Configuration reloaded", "The new settings are active.")
                    }
                    Err(message) => {
                        self.present(
                            Active::ShowError {
                                message: format!(
                                    "Could not reload the configuration:\n\n{message}"
                                ),
                            },
                            ctx,
                        );
                    }
                },
                UiCommand::Quit => {
                    log::info!("quitting");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
            }
        }

        if self.active.is_none() {
            if let Ok(next) = self.incoming.try_recv() {
                self.present(next, ctx);
            }
        }

        // The window close button hides the dialog rather than ending the app,
        // which lives in the tray.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if let Some(active) = self.active.take() {
                active.cancel();
            }
            self.dismiss(ctx);
            return;
        }

        if self.active.is_some() {
            let outcome = self
                .active
                .as_mut()
                .map_or(Outcome::Continue, |active| dialog::show(active, ctx));

            match outcome {
                Outcome::Continue => {}
                Outcome::Replied => {
                    self.active = None;
                    self.dismiss(ctx);
                }
                Outcome::Dismissed => {
                    if let Some(active) = self.active.take() {
                        active.cancel();
                    }
                    self.dismiss(ctx);
                }
            }
        } else if self.visible {
            self.dismiss(ctx);
        }
    }
}

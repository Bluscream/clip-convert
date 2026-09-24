//! Marshalling dialog requests onto the GTK thread.
//!
//! Actions run on worker threads, because typing, converting and network calls
//! all take long enough that doing them on the GTK thread would freeze the tray
//! and the dialogs. But GTK may only be touched from its own thread. A worker
//! that needs an answer therefore sends a [`UiRequest`] carrying a reply channel
//! and blocks on it; the GTK thread shows the dialog and sends the answer back.

use crate::dialog::{self, ActionChoice};
use lcc_core::presets::Preset;
use lcc_core::runner::Prompt;
use std::sync::mpsc::{self, Sender};

/// Something a worker needs the GTK thread to show.
pub enum UiRequest {
    ChooseAction {
        description: String,
        actions: Vec<(String, String)>,
        paste_after: bool,
        reply: Sender<Option<ActionChoice>>,
    },
    AskLimit {
        title: String,
        message: String,
        default: usize,
        reply: Sender<Option<usize>>,
    },
    AskResizeTarget {
        presets: Vec<Preset>,
        reply: Sender<Option<Preset>>,
    },
    ShowError {
        message: String,
    },
}

impl UiRequest {
    /// Shows this request. Must be called on the GTK thread.
    pub fn serve(self) {
        match self {
            Self::ChooseAction {
                description,
                actions,
                paste_after,
                reply,
            } => {
                let choice = dialog::choose_action(&description, &actions, paste_after);
                let _ = reply.send(choice);
            }
            Self::AskLimit {
                title,
                message,
                default,
                reply,
            } => {
                let _ = reply.send(dialog::ask_limit(&title, &message, default));
            }
            Self::AskResizeTarget { presets, reply } => {
                let _ = reply.send(dialog::ask_resize_target(&presets));
            }
            Self::ShowError { message } => dialog::show_error(&message),
        }
    }
}

/// A worker's handle for asking the GTK thread things.
#[derive(Clone)]
pub struct Ui {
    requests: async_channel::Sender<UiRequest>,
}

impl Ui {
    #[must_use]
    pub fn new(requests: async_channel::Sender<UiRequest>) -> Self {
        Self { requests }
    }

    /// Sends a request and waits for the answer.
    ///
    /// Returns `None` if the GTK thread has gone away, which only happens while
    /// shutting down; treating that as "cancelled" is the right behaviour.
    fn ask<T>(&self, build: impl FnOnce(Sender<Option<T>>) -> UiRequest) -> Option<T> {
        let (reply, answer) = mpsc::channel();
        if self.requests.send_blocking(build(reply)).is_err() {
            log::debug!("UI channel closed; treating the prompt as cancelled");
            return None;
        }
        // A closed reply channel means the GTK thread went away mid-prompt,
        // which is shutdown; "cancelled" is the right reading either way.
        answer.recv().ok().flatten()
    }

    /// Shows the action menu for the current clipboard content.
    pub fn choose_action(
        &self,
        description: &str,
        actions: &[(String, String)],
        paste_after: bool,
    ) -> Option<ActionChoice> {
        self.ask(|reply| UiRequest::ChooseAction {
            description: description.to_string(),
            actions: actions.to_vec(),
            paste_after,
            reply,
        })
    }

    /// Reports a failure to the user.
    pub fn error(&self, message: &str) {
        let _ = self.requests.send_blocking(UiRequest::ShowError {
            message: message.to_string(),
        });
    }
}

impl Prompt for Ui {
    fn ask_limit(&self, title: &str, message: &str, default: usize) -> Option<usize> {
        self.ask(|reply| UiRequest::AskLimit {
            title: title.to_string(),
            message: message.to_string(),
            default,
            reply,
        })
    }

    fn ask_resize_target(&self, presets: &[Preset]) -> Option<Preset> {
        self.ask(|reply| UiRequest::AskResizeTarget {
            presets: presets.to_vec(),
            reply,
        })
    }
}

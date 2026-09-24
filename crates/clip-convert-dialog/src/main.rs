//! The dialog process for clip-convert.
//!
//! Reads one [`Request`] as JSON on stdin, shows it, and prints one [`Reply`] as
//! JSON on stdout. Then it exits.
//!
//! Running the graphical toolkit in its own short-lived process is deliberate.
//! Wayland has no operation for hiding a window — winit's `set_visible` is a
//! no-op there — so a long-lived GUI process would always have something on
//! screen. It also means the daemon that sits in the tray all session holds no
//! graphics resources at all.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod ui;

use anyhow::{Context, Result};
use clipconv::protocol::{Reply, Request};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("reading the request from stdin")?;

    let request: Request =
        serde_json::from_str(input.trim()).context("the request was not valid JSON")?;

    let reply = show(request)?;

    let encoded = serde_json::to_string(&reply).context("encoding the reply")?;
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{encoded}").context("writing the reply")?;
    stdout.flush().context("flushing the reply")?;
    Ok(())
}

/// Shows one dialog and returns what the user chose.
fn show(request: Request) -> Result<Reply> {
    // Shared with the egui callback, which cannot return a value directly.
    let outcome = Arc::new(Mutex::new(None));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(request.title())
            .with_inner_size([ui::DIALOG_WIDTH, ui::window_height(&request)])
            .with_resizable(false)
            .with_always_on_top(),
        // Placed by eframe rather than the compositor, so the dialog appears
        // where the user is looking instead of wherever a window would land.
        centered: true,
        ..Default::default()
    };

    let app_outcome = Arc::clone(&outcome);
    eframe::run_native(
        "clip-convert-dialog",
        options,
        Box::new(move |_cc| Ok(Box::new(Dialog::new(request, app_outcome)))),
    )
    .map_err(|e| anyhow::anyhow!("the dialog could not be shown: {e}"))?;

    // Closing the window without choosing is a dismissal, not a failure.
    let reply = outcome
        .lock()
        .map_err(|_| anyhow::anyhow!("the dialog state was poisoned"))?
        .take()
        .unwrap_or(Reply::Cancelled);
    Ok(reply)
}

struct Dialog {
    request: Request,
    state: ui::State,
    outcome: Arc<Mutex<Option<Reply>>>,
}

impl Dialog {
    fn new(request: Request, outcome: Arc<Mutex<Option<Reply>>>) -> Self {
        Self {
            state: ui::State::for_request(&request),
            request,
            outcome,
        }
    }
}

impl eframe::App for Dialog {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(reply) = ui::show(&self.request, &mut self.state, ctx) {
            if let Ok(mut slot) = self.outcome.lock() {
                *slot = Some(reply);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

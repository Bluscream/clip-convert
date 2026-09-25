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
mod widgets;

use anyhow::{Context, Result};
use clipconv::protocol::{Answer, Ask, Reply, Request, WindowSize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("reading the request from stdin")?;

    let ask: Ask = serde_json::from_str(input.trim()).context("the request was not valid JSON")?;

    let answer = show(ask)?;

    let encoded = serde_json::to_string(&answer).context("encoding the reply")?;
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{encoded}").context("writing the reply")?;
    stdout.flush().context("flushing the reply")?;
    Ok(())
}

/// Shows one dialog and returns what the user chose, with the size they left
/// the window at.
fn show(ask: Ask) -> Result<Answer> {
    // Shared with the egui callback, which cannot return a value directly.
    let outcome = Arc::new(Mutex::new(None));
    let final_size = Arc::new(Mutex::new(None));

    // Reopened at whatever size it was last left, falling back to a shape that
    // suits the content.
    let (width, height) = ask.size.map_or_else(
        || {
            (
                ui::window_width(&ask.request),
                ui::window_height(&ask.request),
            )
        },
        |size| {
            #[allow(clippy::cast_precision_loss)] // Window sizes are small integers.
            (size.width as f32, size.height as f32)
        },
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(ask.request.title())
            .with_inner_size([width, height])
            .with_min_inner_size([
                #[allow(clippy::cast_precision_loss)]
                {
                    WindowSize::MIN_WIDTH as f32
                },
                #[allow(clippy::cast_precision_loss)]
                {
                    WindowSize::MIN_HEIGHT as f32
                },
            ])
            .with_resizable(true)
            .with_always_on_top(),
        // Placed by eframe rather than the compositor, so the dialog appears
        // where the user is looking instead of wherever a window would land.
        centered: true,
        ..Default::default()
    };

    let app_outcome = Arc::clone(&outcome);
    let app_size = Arc::clone(&final_size);
    eframe::run_native(
        "clip-convert-dialog",
        options,
        Box::new(move |_cc| Ok(Box::new(Dialog::new(ask.request, app_outcome, app_size)))),
    )
    .map_err(|e| anyhow::anyhow!("the dialog could not be shown: {e}"))?;

    // Closing the window without choosing is a dismissal, not a failure.
    let reply = outcome
        .lock()
        .map_err(|_| anyhow::anyhow!("the dialog state was poisoned"))?
        .take()
        .unwrap_or(Reply::Cancelled);
    let size = final_size.lock().ok().and_then(|size| *size);

    Ok(Answer { reply, size })
}

struct Dialog {
    request: Request,
    state: ui::State,
    outcome: Arc<Mutex<Option<Reply>>>,
    size: Arc<Mutex<Option<WindowSize>>>,
    icons: widgets::Icons,
}

impl Dialog {
    fn new(
        request: Request,
        outcome: Arc<Mutex<Option<Reply>>>,
        size: Arc<Mutex<Option<WindowSize>>>,
    ) -> Self {
        Self {
            state: ui::State::for_request(&request),
            request,
            outcome,
            size,
            icons: widgets::Icons::default(),
        }
    }

    /// Records the current window size, so the last one seen is the one saved.
    ///
    /// Sampled every frame rather than on a resize event: there is no reliable
    /// "window closed at this size" notification, and the cost is a read of
    /// something egui already knows.
    ///
    /// Measured from the drawing area rather than the viewport's `inner_rect`,
    /// which is `None` under Wayland — that value needs the window's position,
    /// and a Wayland compositor does not tell a client where it is.
    fn remember_size(&self, ctx: &egui::Context) {
        let area = ctx.screen_rect().size();
        if let Some(size) = WindowSize::from_points(area.x, area.y) {
            if let Ok(mut slot) = self.size.lock() {
                *slot = Some(size);
            }
        }
    }
}

impl eframe::App for Dialog {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.remember_size(ctx);

        if let Some(reply) = ui::show(&self.request, &mut self.state, &mut self.icons, ctx) {
            if let Ok(mut slot) = self.outcome.lock() {
                *slot = Some(reply);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

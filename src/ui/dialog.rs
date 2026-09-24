//! Drawing the dialogs with egui.
//!
//! Immediate mode suits this app: the action list, its labels and its order all
//! come from the config file, so the menu is a loop over data rather than a set
//! of widgets that have to be built, bound and torn down.

use clipconv::presets::{Fit, Preset};

/// Width of every dialog, in logical points.
const DIALOG_WIDTH: f32 = 460.0;

/// Height of an action button. Deliberately large: these are the primary
/// targets, hit immediately after a hotkey, often without looking closely.
const BUTTON_HEIGHT: f32 = 52.0;

/// Gap between action buttons.
const BUTTON_GAP: f32 = 10.0;

/// Padding around a dialog's contents.
const MARGIN: f32 = 20.0;

/// What the action dialog came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionChoice {
    /// The `id` of the chosen action.
    pub action_id: String,
    /// The state of the checkbox when the choice was made.
    pub paste_after: bool,
}

/// A dialog currently being shown, and the state it is collecting.
pub enum Active {
    ChooseAction {
        description: String,
        actions: Vec<(String, String)>,
        paste_after: bool,
        reply: std::sync::mpsc::Sender<Option<ActionChoice>>,
    },
    AskLimit {
        title: String,
        message: String,
        value: String,
        reply: std::sync::mpsc::Sender<Option<usize>>,
    },
    AskResizeTarget {
        presets: Vec<Preset>,
        custom: Option<CustomSize>,
        reply: std::sync::mpsc::Sender<Option<Preset>>,
    },
    ShowError {
        message: String,
    },
}

/// The fields of the custom-size form.
pub struct CustomSize {
    pub width: String,
    pub height: String,
    pub max_kb: String,
    pub format: String,
    pub exact: bool,
}

impl Default for CustomSize {
    fn default() -> Self {
        Self {
            width: "512".to_string(),
            height: "512".to_string(),
            max_kb: "0".to_string(),
            format: "png".to_string(),
            exact: false,
        }
    }
}

impl Active {
    /// The window title for this dialog.
    #[must_use]
    pub fn title(&self) -> &str {
        match self {
            Self::ChooseAction { .. } => "Clipboard actions",
            Self::AskLimit { title, .. } => title,
            Self::AskResizeTarget { .. } => "Resize image",
            Self::ShowError { .. } => "Something went wrong",
        }
    }

    /// How tall the window should be for this dialog's content.
    #[must_use]
    pub fn height(&self) -> f32 {
        match self {
            Self::ChooseAction { actions, .. } => {
                #[allow(clippy::cast_precision_loss)] // A menu never has enough entries to matter.
                let rows = actions.len() as f32;
                rows.mul_add(BUTTON_HEIGHT + BUTTON_GAP, 150.0)
            }
            Self::AskLimit { .. } => 190.0,
            Self::AskResizeTarget {
                presets, custom, ..
            } => {
                if custom.is_some() {
                    300.0
                } else {
                    #[allow(clippy::cast_precision_loss)]
                    let rows = presets.len() as f32 + 1.0;
                    rows.mul_add(BUTTON_HEIGHT + BUTTON_GAP, 120.0).min(760.0)
                }
            }
            Self::ShowError { .. } => 220.0,
        }
    }

    /// Notifies the waiting worker that the user dismissed the dialog.
    pub fn cancel(self) {
        match self {
            Self::ChooseAction {
                paste_after, reply, ..
            } => {
                // The checkbox is still reported, so toggling it and pressing
                // Escape still remembers the setting.
                let _ = reply.send(None);
                let _ = paste_after;
            }
            Self::AskLimit { reply, .. } => {
                let _ = reply.send(None);
            }
            Self::AskResizeTarget { reply, .. } => {
                let _ = reply.send(None);
            }
            Self::ShowError { .. } => {}
        }
    }
}

/// Whether a dialog finished, and whether it already answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Still open.
    Continue,
    /// The user answered; the reply has already been sent.
    Replied,
    /// The user dismissed it; the caller still owes the worker a `None`.
    Dismissed,
}

/// Draws the active dialog. Returns whether it is finished.
pub fn show(active: &mut Active, ctx: &egui::Context) -> Outcome {
    let mut outcome = Outcome::Continue;

    egui::CentralPanel::default()
        .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(MARGIN))
        .show(ctx, |ui| {
            outcome = match active {
                Active::ChooseAction {
                    description,
                    actions,
                    paste_after,
                    reply,
                } => choose_action(ui, description, actions, paste_after, reply),
                Active::AskLimit {
                    message,
                    value,
                    reply,
                    ..
                } => ask_limit(ui, message, value, reply),
                Active::AskResizeTarget {
                    presets,
                    custom,
                    reply,
                } => ask_resize_target(ui, presets, custom, reply),
                Active::ShowError { message } => show_error(ui, message),
            };
        });

    // Escape always dismisses, which is what a modal prompt over someone else's
    // work should do.
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return Outcome::Dismissed;
    }
    outcome
}

/// A full-width button sized for quick, confident clicking.
fn wide_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let text = egui::RichText::new(label).size(16.0).strong();
    ui.add_sized(
        [ui.available_width(), BUTTON_HEIGHT],
        egui::Button::new(text),
    )
}

/// A bold line summarising what is on the clipboard.
fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(15.0).strong());
    ui.add_space(12.0);
}

fn choose_action(
    ui: &mut egui::Ui,
    description: &str,
    actions: &[(String, String)],
    paste_after: &mut bool,
    reply: &std::sync::mpsc::Sender<Option<ActionChoice>>,
) -> Outcome {
    heading(ui, description);

    let mut chosen = None;
    for (id, label) in actions {
        if wide_button(ui, label).clicked() {
            chosen = Some(id.clone());
        }
        ui.add_space(BUTTON_GAP);
    }

    ui.add_space(4.0);
    ui.checkbox(paste_after, "Paste after action");

    if let Some(action_id) = chosen {
        let _ = reply.send(Some(ActionChoice {
            action_id,
            paste_after: *paste_after,
        }));
        return Outcome::Replied;
    }
    Outcome::Continue
}

fn ask_limit(
    ui: &mut egui::Ui,
    message: &str,
    value: &mut String,
    reply: &std::sync::mpsc::Sender<Option<usize>>,
) -> Outcome {
    heading(ui, message);

    let entry = ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Monospace),
    );
    // Only claimed when nothing else holds focus: requesting it every frame
    // would keep the app repainting instead of idling.
    if ui.memory(|m| m.focused().is_none()) {
        entry.request_focus();
    }

    let parsed = value.trim().parse::<usize>().ok().filter(|v| *v > 0);
    if parsed.is_none() && !value.trim().is_empty() {
        ui.colored_label(
            ui.visuals().error_fg_color,
            "Enter a whole number above zero.",
        );
    }

    ui.add_space(14.0);
    let submitted = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

    let mut done = Outcome::Continue;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                parsed.is_some(),
                egui::Button::new("Continue").min_size([120.0, 34.0].into()),
            )
            .clicked()
            || (submitted && parsed.is_some())
        {
            let _ = reply.send(parsed);
            done = Outcome::Replied;
        }
        if ui
            .add(egui::Button::new("Cancel").min_size([100.0, 34.0].into()))
            .clicked()
        {
            let _ = reply.send(None);
            done = Outcome::Replied;
        }
    });
    done
}

fn ask_resize_target(
    ui: &mut egui::Ui,
    presets: &[Preset],
    custom: &mut Option<CustomSize>,
    reply: &std::sync::mpsc::Sender<Option<Preset>>,
) -> Outcome {
    if let Some(fields) = custom.as_mut() {
        return custom_size(ui, fields, reply);
    }

    heading(ui, "Resize to:");

    let mut chosen = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for preset in presets {
            // Two lines: the name, and the limits it encodes, so choosing does
            // not require remembering each platform's rules.
            let label = format!("{}\n{}", preset.label, preset.summary());
            let text = egui::RichText::new(label).size(14.0);
            if ui
                .add_sized(
                    [ui.available_width(), BUTTON_HEIGHT],
                    egui::Button::new(text),
                )
                .clicked()
            {
                chosen = Some(preset.clone());
            }
            ui.add_space(BUTTON_GAP);
        }

        if wide_button(ui, "Custom size…").clicked() {
            *custom = Some(CustomSize::default());
        }
    });

    if let Some(preset) = chosen {
        let _ = reply.send(Some(preset));
        return Outcome::Replied;
    }
    Outcome::Continue
}

fn custom_size(
    ui: &mut egui::Ui,
    fields: &mut CustomSize,
    reply: &std::sync::mpsc::Sender<Option<Preset>>,
) -> Outcome {
    heading(ui, "Fit the image to:");

    let row = |ui: &mut egui::Ui, caption: &str, value: &mut String| {
        ui.horizontal(|ui| {
            ui.add_sized([170.0, 24.0], egui::Label::new(caption));
            ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
        });
    };
    row(ui, "Width (px)", &mut fields.width);
    row(ui, "Height (px)", &mut fields.height);
    row(ui, "Max size (KB, 0 = any)", &mut fields.max_kb);

    ui.horizontal(|ui| {
        ui.add_sized([170.0, 24.0], egui::Label::new("Format"));
        egui::ComboBox::from_id_salt("format")
            .selected_text(fields.format.to_uppercase())
            .show_ui(ui, |ui| {
                for name in ["png", "webp", "jpeg", "gif"] {
                    ui.selectable_value(&mut fields.format, name.to_string(), name.to_uppercase());
                }
            });
    });

    ui.add_space(8.0);
    ui.checkbox(&mut fields.exact, "Pad to exactly this canvas");
    ui.add_space(14.0);

    let width = fields.width.trim().parse::<u32>().ok().filter(|v| *v > 0);
    let height = fields.height.trim().parse::<u32>().ok().filter(|v| *v > 0);
    let max_kb = fields.max_kb.trim().parse::<u64>().ok();
    let valid = width.is_some() && height.is_some() && max_kb.is_some();
    if !valid {
        ui.colored_label(
            ui.visuals().error_fg_color,
            "Width and height must be above zero, and the size a whole number.",
        );
    }

    let mut done = Outcome::Continue;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                valid,
                egui::Button::new("Resize").min_size([120.0, 34.0].into()),
            )
            .clicked()
        {
            let _ = reply.send(Some(Preset {
                id: "custom".to_string(),
                label: "Custom".to_string(),
                width: width.unwrap_or(512),
                height: height.unwrap_or(512),
                max_bytes: max_kb.unwrap_or(0) * 1024,
                format: fields.format.clone(),
                fit: if fields.exact {
                    Fit::Exact
                } else {
                    Fit::Inside
                },
            }));
            done = Outcome::Replied;
        }
        if ui
            .add(egui::Button::new("Cancel").min_size([100.0, 34.0].into()))
            .clicked()
        {
            let _ = reply.send(None);
            done = Outcome::Replied;
        }
    });
    done
}

fn show_error(ui: &mut egui::Ui, message: &str) -> Outcome {
    ui.label(egui::RichText::new(message).size(14.0));
    ui.add_space(16.0);
    if ui
        .add(egui::Button::new("Close").min_size([120.0, 34.0].into()))
        .clicked()
    {
        return Outcome::Replied;
    }
    Outcome::Continue
}

/// The window size a dialog wants.
#[must_use]
pub fn window_size(active: &Active) -> egui::Vec2 {
    egui::vec2(DIALOG_WIDTH, active.height())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions(n: usize) -> Vec<(String, String)> {
        (0..n)
            .map(|i| (format!("id{i}"), format!("Action {i}")))
            .collect()
    }

    #[test]
    fn a_menu_grows_with_the_number_of_actions() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let small = Active::ChooseAction {
            description: "Text".to_string(),
            actions: actions(2),
            paste_after: true,
            reply: tx.clone(),
        };
        let large = Active::ChooseAction {
            description: "Text".to_string(),
            actions: actions(6),
            paste_after: true,
            reply: tx,
        };
        assert!(large.height() > small.height());
        assert!((window_size(&small).x - DIALOG_WIDTH).abs() < f32::EPSILON);
    }

    #[test]
    fn a_long_preset_list_does_not_produce_an_unusable_window() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let many = Active::AskResizeTarget {
            presets: clipconv::presets::factory(),
            custom: None,
            reply: tx,
        };
        assert!(many.height() <= 760.0, "got {}", many.height());
    }

    #[test]
    fn every_dialog_has_a_title() {
        let (tx, _rx) = std::sync::mpsc::channel::<Option<usize>>();
        let limit = Active::AskLimit {
            title: "Split".to_string(),
            message: "How many?".to_string(),
            value: "2000".to_string(),
            reply: tx,
        };
        assert_eq!(limit.title(), "Split");
        assert_eq!(
            Active::ShowError {
                message: "x".to_string()
            }
            .title(),
            "Something went wrong"
        );
    }

    #[test]
    fn cancelling_tells_the_waiting_worker() {
        let (tx, rx) = std::sync::mpsc::channel();
        Active::AskLimit {
            title: "Split".to_string(),
            message: "How many?".to_string(),
            value: "2000".to_string(),
            reply: tx,
        }
        .cancel();
        assert_eq!(rx.recv().expect("a reply"), None);
    }

    #[test]
    fn custom_size_defaults_are_usable_as_typed() {
        let fields = CustomSize::default();
        assert_eq!(fields.width.parse::<u32>().ok(), Some(512));
        assert_eq!(fields.max_kb.parse::<u64>().ok(), Some(0));
        assert!(clipconv::encode::Format::parse(&fields.format).is_some());
    }
}

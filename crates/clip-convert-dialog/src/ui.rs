//! Drawing the dialogs.
//!
//! Immediate mode suits this app: the action list, its labels and its order all
//! come from the config file, so the menu is a loop over data rather than a set
//! of widgets that have to be built, bound and torn down.

use clipconv::presets::{Fit, Preset};
use clipconv::protocol::{ActionChoice, Reply, Request};

/// Width of a dialog, in logical points.
const DIALOG_WIDTH: f32 = 460.0;

/// Width of the resize chooser, which lays its targets out in two columns.
const WIDE_DIALOG_WIDTH: f32 = 680.0;

/// How many columns the resize chooser uses.
const PRESET_COLUMNS: usize = 2;

/// Size of a preset's name.
const PRESET_TITLE: f32 = 15.0;

/// Size of the limits line under a preset's name. Deliberately smaller: it is
/// reference information, not the thing being chosen.
const PRESET_SUBTITLE: f32 = 11.5;

/// Minimum height of an action button. Deliberately large: these are the
/// primary targets, hit immediately after a hotkey, often without looking
/// closely. A button grows past this when its label needs two lines.
const BUTTON_HEIGHT: f32 = 58.0;

/// Size of the text on an action button.
const BUTTON_TEXT: f32 = 18.0;

/// Gap between action buttons.
const BUTTON_GAP: f32 = 12.0;

/// Padding around a dialog's contents.
const MARGIN: f32 = 22.0;

/// Mutable state a dialog collects while it is open.
pub enum State {
    ChooseAction { paste_after: bool },
    AskLimit { value: String },
    AskResizeTarget { custom: Option<CustomSize> },
    ShowError,
}

impl State {
    /// The starting state for a request.
    #[must_use]
    pub fn for_request(request: &Request) -> Self {
        match request {
            Request::ChooseAction { paste_after, .. } => Self::ChooseAction {
                paste_after: *paste_after,
            },
            Request::AskLimit { default, .. } => Self::AskLimit {
                value: default.to_string(),
            },
            Request::AskResizeTarget { .. } => Self::AskResizeTarget { custom: None },
            Request::ShowError { .. } => Self::ShowError,
        }
    }
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

/// How wide the window should be for a request's content.
#[must_use]
pub fn window_width(request: &Request) -> f32 {
    match request {
        Request::AskResizeTarget { .. } => WIDE_DIALOG_WIDTH,
        _ => DIALOG_WIDTH,
    }
}

/// How tall the window should be for a request's content.
#[must_use]
pub fn window_height(request: &Request) -> f32 {
    match request {
        Request::ChooseAction { actions, .. } => {
            #[allow(clippy::cast_precision_loss)] // A menu never has enough entries to matter.
            let rows = actions.len() as f32;
            rows.mul_add(BUTTON_HEIGHT + BUTTON_GAP, 150.0)
        }
        Request::AskLimit { .. } => 200.0,
        Request::AskResizeTarget { presets } => {
            // Two per row, plus a row for the custom-size button.
            let rows = presets.len().div_ceil(PRESET_COLUMNS) + 1;
            #[allow(clippy::cast_precision_loss)]
            let rows = rows as f32;
            // Capped so a long preset list cannot produce a window taller than
            // the screen; the list scrolls instead.
            rows.mul_add(BUTTON_HEIGHT + BUTTON_GAP, 120.0).min(720.0)
        }
        Request::ShowError { .. } => 230.0,
    }
}

/// Draws the dialog. Returns a reply once the user has finished with it.
pub fn show(request: &Request, state: &mut State, ctx: &egui::Context) -> Option<Reply> {
    let mut reply = None;

    egui::CentralPanel::default()
        .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(MARGIN))
        .show(ctx, |ui| {
            reply = match (request, state) {
                (
                    Request::ChooseAction {
                        description,
                        actions,
                        ..
                    },
                    State::ChooseAction { paste_after },
                ) => choose_action(ui, description, actions, paste_after),
                (Request::AskLimit { message, .. }, State::AskLimit { value }) => {
                    ask_limit(ui, message, value)
                }
                (Request::AskResizeTarget { presets }, State::AskResizeTarget { custom }) => {
                    ask_resize_target(ui, presets, custom)
                }
                (Request::ShowError { message }, State::ShowError) => show_error(ui, message),
                // The state is always built from the request, so this cannot
                // happen; dismissing is the safe answer if it ever did.
                _ => Some(Reply::Cancelled),
            };
        });

    // Escape always dismisses, which is what a prompt over someone else's work
    // should do.
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return Some(Reply::Cancelled);
    }
    reply
}

/// A full-width button sized for quick, confident clicking.
///
/// `add_sized` rather than `min_size`, because only the former centres the
/// label; a button given a minimum size draws its text against the left edge.
/// Wrapping is on so a long label from a custom action folds onto a second line
/// instead of running off the edge.
fn wide_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let text = egui::RichText::new(label).size(BUTTON_TEXT).strong();
    let width = ui.available_width();
    ui.add_sized([width, BUTTON_HEIGHT], egui::Button::new(text).wrap())
}

/// One preset: its name, with the limits it encodes underneath in smaller,
/// dimmer text.
///
/// A `LayoutJob` rather than one string, because the two lines need different
/// sizes and weights — the name is what is being chosen, the limits are there
/// so choosing does not require remembering each platform's rules.
fn preset_button(ui: &mut egui::Ui, preset: &Preset) -> egui::Response {
    let mut text = egui::text::LayoutJob::default();
    text.append(
        &preset.label,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(PRESET_TITLE),
            color: ui.visuals().strong_text_color(),
            ..Default::default()
        },
    );
    text.append(
        &format!("\n{}", preset.summary()),
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(PRESET_SUBTITLE),
            // Dimmed from the body colour rather than `weak_text_color`,
            // which is faint enough to be hard to read at this size.
            color: ui.visuals().text_color().gamma_multiply(0.82),
            ..Default::default()
        },
    );

    let width = ui.available_width();
    ui.add_sized([width, BUTTON_HEIGHT], egui::Button::new(text).wrap())
}

/// A bold line summarising what is on the clipboard.
fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(15.0).strong());
    ui.add_space(14.0);
}

fn choose_action(
    ui: &mut egui::Ui,
    description: &str,
    actions: &[(String, String)],
    paste_after: &mut bool,
) -> Option<Reply> {
    heading(ui, description);

    let mut chosen = None;
    // The checkbox is anchored to the bottom so the list above it can take the
    // remaining space and scroll, rather than pushing it off the window.
    egui::TopBottomPanel::bottom("paste")
        .frame(egui::Frame::none().outer_margin(egui::Margin {
            top: 10.0,
            ..egui::Margin::ZERO
        }))
        .show_inside(ui, |ui| {
            ui.checkbox(paste_after, "Paste after action");
        });

    egui::ScrollArea::vertical().show(ui, |ui| {
        for (id, label) in actions {
            if wide_button(ui, label).clicked() {
                chosen = Some(id.clone());
            }
            ui.add_space(BUTTON_GAP);
        }
    });

    chosen.map(|action_id| Reply::Action {
        choice: ActionChoice {
            action_id,
            paste_after: *paste_after,
        },
    })
}

fn ask_limit(ui: &mut egui::Ui, message: &str, value: &mut String) -> Option<Reply> {
    heading(ui, message);

    let entry = ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Monospace),
    );
    // Only claimed when nothing else holds focus: requesting it every frame
    // would keep the process repainting instead of idling.
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

    let submitted = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    ui.add_space(16.0);

    let mut reply = None;
    ui.horizontal(|ui| {
        let accept = ui.add_enabled(
            parsed.is_some(),
            egui::Button::new("Continue").min_size([130.0, 36.0].into()),
        );
        if let Some(value) = parsed {
            if accept.clicked() || submitted {
                reply = Some(Reply::Limit { value });
            }
        }
        if ui
            .add(egui::Button::new("Cancel").min_size([110.0, 36.0].into()))
            .clicked()
        {
            reply = Some(Reply::Cancelled);
        }
    });
    reply
}

fn ask_resize_target(
    ui: &mut egui::Ui,
    presets: &[Preset],
    custom: &mut Option<CustomSize>,
) -> Option<Reply> {
    if let Some(fields) = custom.as_mut() {
        return custom_size(ui, fields);
    }

    heading(ui, "Resize to:");

    let mut chosen = None;
    let mut open_custom = false;
    egui::ScrollArea::vertical().show(ui, |ui| {
        // Laid out in rows of two: the list is long enough that one column
        // means most of it is off-screen.
        for row in presets.chunks(PRESET_COLUMNS) {
            ui.columns(PRESET_COLUMNS, |columns| {
                for (column, preset) in columns.iter_mut().zip(row) {
                    if preset_button(column, preset).clicked() {
                        chosen = Some(preset.clone());
                    }
                }
            });
            ui.add_space(BUTTON_GAP);
        }
        if wide_button(ui, "Custom size…").clicked() {
            open_custom = true;
        }
    });

    if open_custom {
        *custom = Some(CustomSize::default());
    }
    chosen.map(|preset| Reply::ResizeTarget {
        preset: Box::new(preset),
    })
}

fn custom_size(ui: &mut egui::Ui, fields: &mut CustomSize) -> Option<Reply> {
    heading(ui, "Fit the image to:");

    let row = |ui: &mut egui::Ui, caption: &str, value: &mut String| {
        ui.horizontal(|ui| {
            ui.add_sized([180.0, 24.0], egui::Label::new(caption));
            ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
        });
    };
    row(ui, "Width (px)", &mut fields.width);
    row(ui, "Height (px)", &mut fields.height);
    row(ui, "Max size (KB, 0 = any)", &mut fields.max_kb);

    ui.horizontal(|ui| {
        ui.add_sized([180.0, 24.0], egui::Label::new("Format"));
        egui::ComboBox::from_id_salt("format")
            .selected_text(fields.format.to_uppercase())
            .show_ui(ui, |ui| {
                for name in ["png", "webp", "jpeg", "gif"] {
                    ui.selectable_value(&mut fields.format, name.to_string(), name.to_uppercase());
                }
            });
    });

    ui.add_space(10.0);
    ui.checkbox(&mut fields.exact, "Pad to exactly this canvas");
    ui.add_space(14.0);

    let width = fields.width.trim().parse::<u32>().ok().filter(|v| *v > 0);
    let height = fields.height.trim().parse::<u32>().ok().filter(|v| *v > 0);
    let max_kb = fields.max_kb.trim().parse::<u64>().ok();

    if width.is_none() || height.is_none() || max_kb.is_none() {
        ui.colored_label(
            ui.visuals().error_fg_color,
            "Width and height must be above zero, and the size a whole number.",
        );
    }

    let mut reply = None;
    ui.horizontal(|ui| {
        let ready = width.is_some() && height.is_some() && max_kb.is_some();
        if ui
            .add_enabled(
                ready,
                egui::Button::new("Resize").min_size([130.0, 36.0].into()),
            )
            .clicked()
        {
            if let (Some(width), Some(height), Some(max_kb)) = (width, height, max_kb) {
                reply = Some(Reply::ResizeTarget {
                    preset: Box::new(Preset {
                        id: "custom".to_string(),
                        label: "Custom".to_string(),
                        width,
                        height,
                        max_bytes: max_kb * 1024,
                        format: fields.format.clone(),
                        fit: if fields.exact {
                            Fit::Exact
                        } else {
                            Fit::Inside
                        },
                    }),
                });
            }
        }
        if ui
            .add(egui::Button::new("Cancel").min_size([110.0, 36.0].into()))
            .clicked()
        {
            reply = Some(Reply::Cancelled);
        }
    });
    reply
}

fn show_error(ui: &mut egui::Ui, message: &str) -> Option<Reply> {
    ui.label(egui::RichText::new(message).size(14.0));
    ui.add_space(18.0);
    ui.add(egui::Button::new("Close").min_size([130.0, 36.0].into()))
        .clicked()
        .then_some(Reply::Acknowledged)
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
        let small = Request::ChooseAction {
            description: "Text".to_string(),
            actions: actions(2),
            paste_after: true,
        };
        let large = Request::ChooseAction {
            description: "Text".to_string(),
            actions: actions(6),
            paste_after: true,
        };
        assert!(window_height(&large) > window_height(&small));
    }

    #[test]
    fn a_long_preset_list_does_not_produce_an_unusable_window() {
        let many = Request::AskResizeTarget {
            presets: clipconv::presets::factory(),
        };
        assert!(
            window_height(&many) <= 720.0,
            "got {}",
            window_height(&many)
        );
    }

    #[test]
    fn every_request_produces_matching_state() {
        let requests = [
            Request::ChooseAction {
                description: String::new(),
                actions: actions(1),
                paste_after: false,
            },
            Request::AskLimit {
                title: String::new(),
                message: String::new(),
                default: 42,
            },
            Request::AskResizeTarget {
                presets: Vec::new(),
            },
            Request::ShowError {
                message: String::new(),
            },
        ];
        for request in &requests {
            let state = State::for_request(request);
            // The pairing in `show` must never fall through to the catch-all.
            let paired = matches!(
                (request, &state),
                (Request::ChooseAction { .. }, State::ChooseAction { .. })
                    | (Request::AskLimit { .. }, State::AskLimit { .. })
                    | (
                        Request::AskResizeTarget { .. },
                        State::AskResizeTarget { .. }
                    )
                    | (Request::ShowError { .. }, State::ShowError)
            );
            assert!(paired, "{request:?} did not pair with its state");
        }
    }

    #[test]
    fn the_limit_field_starts_at_the_default() {
        let state = State::for_request(&Request::AskLimit {
            title: String::new(),
            message: String::new(),
            default: 2000,
        });
        match state {
            State::AskLimit { value } => assert_eq!(value, "2000"),
            _ => panic!("wrong state"),
        }
    }

    #[test]
    fn the_checkbox_starts_from_the_remembered_setting() {
        for remembered in [true, false] {
            let state = State::for_request(&Request::ChooseAction {
                description: String::new(),
                actions: actions(1),
                paste_after: remembered,
            });
            match state {
                State::ChooseAction { paste_after } => assert_eq!(paste_after, remembered),
                _ => panic!("wrong state"),
            }
        }
    }

    #[test]
    fn custom_size_defaults_are_usable_as_typed() {
        let fields = CustomSize::default();
        assert_eq!(fields.width.parse::<u32>().ok(), Some(512));
        assert_eq!(fields.max_kb.parse::<u64>().ok(), Some(0));
        assert!(clipconv::encode::Format::parse(&fields.format).is_some());
    }
}

//! Drawing the dialogs.
//!
//! Immediate mode suits this app: the action list, its labels and its order all
//! come from the config file, so the menu is a loop over data rather than a set
//! of widgets that have to be built, bound and torn down.

use crate::widgets::{
    accepted, bottom_bar, close_button, color, dialog_buttons, dismiss_button, focus_first,
    heading, history_field, icon_button, navigate, wide_button, Choice, Icons, Style,
    BUTTON_HEIGHT, BUTTON_TEXT,
};
use clipconv::presets::{Fit, Preset};
use clipconv::protocol::{ActionChoice, ActionEntry, Reply, Request};

/// Width of a dialog, in logical points.
const DIALOG_WIDTH: f32 = 460.0;

/// Width of the resize chooser, which lays its targets out in two columns.
const WIDE_DIALOG_WIDTH: f32 = 680.0;

/// How many columns the resize chooser opens with. It uses more when the
/// window is dragged wider, so the targets fill the space rather than leaving a
/// column of nothing beside them.
const PRESET_COLUMNS: usize = 2;

/// Width one column of targets needs to stay readable.
const PRESET_COLUMN_WIDTH: f32 = 300.0;

/// Most columns worth using: past this the labels are further apart than they
/// are wide, and the list stops reading as a list.
const PRESET_COLUMNS_MAX: usize = 4;

/// Size of a preset's name.
const PRESET_TITLE: f32 = 15.0;

/// Size of the limits line under a preset's name. Deliberately smaller: it is
/// reference information, not the thing being chosen.
const PRESET_SUBTITLE: f32 = 11.5;

/// Gap between action buttons.
const BUTTON_GAP: f32 = 12.0;

/// Padding around a dialog's contents.
const MARGIN: f32 = 22.0;

/// Mutable state a dialog collects while it is open.
pub enum State {
    ChooseAction {
        paste_after: bool,
        selected: usize,
    },
    AskLimit {
        value: String,
    },
    AskResizeTarget {
        custom: Option<CustomSize>,
        selected: usize,
    },
    AskReplace {
        fields: ReplaceFields,
    },
    AskConversion {
        selected: usize,
    },
    AskVideoTarget {
        fields: VideoFields,
    },
    ShowError,
}

/// The fields of the video form. All optional: what is left blank is left
/// alone, which is the point of the dialog.
#[derive(Default)]
pub struct VideoFields {
    pub width: String,
    pub height: String,
    pub max_size: String,
    pub length: String,
}

impl VideoFields {
    /// What was typed, as a target. `None` when something was typed that does
    /// not read as a number, so the dialog can say so rather than drop it.
    fn parse(&self) -> Option<clipconv::video::VideoTarget> {
        let pixels = |text: &str| -> Option<Option<u32>> {
            if text.trim().is_empty() {
                return Some(None);
            }
            text.trim().parse::<u32>().ok().filter(|v| *v > 0).map(Some)
        };
        let optional = |text: &str, parse: &dyn Fn(&str) -> Option<f64>| -> Option<Option<f64>> {
            if text.trim().is_empty() {
                return Some(None);
            }
            parse(text).map(Some)
        };

        Some(clipconv::video::VideoTarget {
            width: pixels(&self.width)?,
            height: pixels(&self.height)?,
            max_bytes: if self.max_size.trim().is_empty() {
                None
            } else {
                Some(clipconv::content::parse_bytes(&self.max_size)?)
            },
            seconds: optional(&self.length, &|text| clipconv::video::parse_seconds(text))?,
        })
    }
}

/// The fields of the find-and-replace form.
#[derive(Default)]
pub struct ReplaceFields {
    pub pattern: String,
    pub replacement: String,
}

impl State {
    /// The starting state for a request.
    #[must_use]
    pub fn for_request(request: &Request) -> Self {
        match request {
            Request::ChooseAction { paste_after, .. } => Self::ChooseAction {
                paste_after: *paste_after,
                selected: 0,
            },
            Request::AskLimit { default, .. } => Self::AskLimit {
                value: default.to_string(),
            },
            Request::AskResizeTarget { .. } => Self::AskResizeTarget {
                custom: None,
                selected: 0,
            },
            Request::AskConversion { .. } => Self::AskConversion { selected: 0 },
            Request::AskVideoTarget { .. } => Self::AskVideoTarget {
                fields: VideoFields::default(),
            },
            // Pre-filled with the last pattern used: repeating the previous
            // replacement on a new piece of text is the common case.
            Request::AskReplace {
                patterns,
                replacements,
            } => Self::AskReplace {
                fields: ReplaceFields {
                    pattern: patterns.first().cloned().unwrap_or_default(),
                    replacement: replacements.first().cloned().unwrap_or_default(),
                },
            },
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
        Request::AskReplace { .. } => 260.0,
        Request::AskVideoTarget { .. } => 330.0,
        Request::AskConversion { options, .. } => {
            #[allow(clippy::cast_precision_loss)] // Never enough entries to matter.
            let rows = options.len() as f32;
            rows.mul_add(BUTTON_HEIGHT + BUTTON_GAP, 130.0).min(720.0)
        }
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
pub fn show(
    request: &Request,
    state: &mut State,
    icons: &mut Icons,
    ctx: &egui::Context,
) -> Option<Reply> {
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
                    State::ChooseAction {
                        paste_after,
                        selected,
                    },
                ) => choose_action(ui, description, actions, paste_after, selected, icons),
                (Request::AskLimit { message, .. }, State::AskLimit { value }) => {
                    ask_limit(ui, message, value)
                }
                (
                    Request::AskResizeTarget { presets },
                    State::AskResizeTarget { custom, selected },
                ) => ask_resize_target(ui, presets, custom, selected, icons),
                (
                    Request::AskReplace {
                        patterns,
                        replacements,
                    },
                    State::AskReplace { fields },
                ) => ask_replace(ui, patterns, replacements, fields),
                (Request::AskConversion { source, options }, State::AskConversion { selected }) => {
                    ask_conversion(ui, source, options, selected, icons)
                }
                (Request::AskVideoTarget { subject }, State::AskVideoTarget { fields }) => {
                    ask_video_target(ui, subject, fields)
                }
                (Request::ShowError { message }, State::ShowError) => show_error(ui, message),
                // The state is always built from the request, so this cannot
                // happen; dismissing is the safe answer if it ever did.
                _ => Some(Reply::Cancelled),
            };
        });

    // Escape always dismisses, which is what a prompt over someone else's work
    // should do — and the corner ✕ says so on screen, for a compositor that
    // gives the window no title bar to close it with.
    if close_button(ctx) || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return Some(Reply::Cancelled);
    }
    reply
}

/// One preset: its name, with the limits it encodes underneath in smaller,
/// dimmer text.
///
/// A `LayoutJob` rather than one string, because the two lines need different
/// sizes and weights — the name is what is being chosen, the limits are there
/// so choosing does not require remembering each platform's rules.
fn preset_button(
    ui: &mut egui::Ui,
    icons: &mut Icons,
    preset: &Preset,
    selected: bool,
) -> egui::Response {
    let title =
        color(preset.text_color.as_ref()).unwrap_or_else(|| ui.visuals().strong_text_color());
    let mut text = egui::text::LayoutJob::default();
    text.append(
        &preset.label,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(PRESET_TITLE),
            color: title,
            ..Default::default()
        },
    );
    text.append(
        &format!("\n{}", preset.summary()),
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(PRESET_SUBTITLE),
            // Dimmed from the title colour rather than `weak_text_color`,
            // which is faint enough to be hard to read at this size.
            color: title.gamma_multiply(0.82),
            ..Default::default()
        },
    );

    icon_button(
        ui,
        icons,
        &preset.id,
        Style::for_preset(preset).selected(selected),
        text,
    )
}

fn choose_action(
    ui: &mut egui::Ui,
    description: &str,
    actions: &[ActionEntry],
    paste_after: &mut bool,
    selected: &mut usize,
    icons: &mut Icons,
) -> Option<Reply> {
    heading(ui, description);

    let mut chosen = navigate(ui.ctx(), actions.len(), selected)
        .and_then(|index| actions.get(index))
        .map(|action| action.id.clone());
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
        for (index, action) in actions.iter().enumerate() {
            // Set on the text rather than left to the painter's fallback:
            // `strong()` gives the galley an explicit colour, which a fallback
            // can no longer override.
            let mut text = egui::RichText::new(&action.label)
                .size(BUTTON_TEXT)
                .strong();
            if let Some(chosen) = color(action.text_color.as_ref()) {
                text = text.color(chosen);
            }
            let style = Style::for_action(action).selected(index == *selected);
            if icon_button(ui, icons, &action.id, style, text).clicked() {
                chosen = Some(action.id.clone());
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
    let parsed = value.trim().parse::<usize>().ok().filter(|v| *v > 0);

    // The buttons are placed first so they own the bottom edge; the form above
    // then takes whatever is left, however tall the window is.
    let choice = bottom_bar(ui, "limit-actions", |ui| {
        dialog_buttons(ui, "Continue", parsed.is_some())
    });

    heading(ui, message);
    let entry = ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Monospace),
    );
    focus_first(ui, &entry);

    if parsed.is_none() && !value.trim().is_empty() {
        ui.colored_label(
            ui.visuals().error_fg_color,
            "Enter a whole number above zero.",
        );
    }

    let submitted = accepted(ui.ctx());
    match (choice, parsed) {
        (Some(Choice::Accept), Some(value)) => Some(Reply::Limit { value }),
        (Some(Choice::Cancel), _) => Some(Reply::Cancelled),
        _ if submitted => parsed.map(|value| Reply::Limit { value }),
        _ => None,
    }
}

fn ask_resize_target(
    ui: &mut egui::Ui,
    presets: &[Preset],
    custom: &mut Option<CustomSize>,
    selected: &mut usize,
    icons: &mut Icons,
) -> Option<Reply> {
    if let Some(fields) = custom.as_mut() {
        return custom_size(ui, fields);
    }

    heading(ui, "Resize to:");

    // Laid out in columns because the list is long enough that one column means
    // most of it is off-screen — and in as many as the window is wide enough
    // for, so resizing makes the list fit rather than leaving empty space.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    // A window is never wide enough for this to leave the range.
    let column_count =
        ((ui.available_width() / PRESET_COLUMN_WIDTH) as usize).clamp(1, PRESET_COLUMNS_MAX);

    let mut chosen = navigate(ui.ctx(), presets.len(), selected)
        .and_then(|index| presets.get(index))
        .cloned();
    let mut open_custom = false;

    egui::ScrollArea::vertical().show(ui, |ui| {
        for (row_index, row) in presets.chunks(column_count).enumerate() {
            ui.columns(column_count, |columns| {
                for (offset, (column, preset)) in columns.iter_mut().zip(row).enumerate() {
                    let index = row_index * column_count + offset;
                    if preset_button(column, icons, preset, index == *selected).clicked() {
                        chosen = Some(preset.clone());
                    }
                }
            });
            ui.add_space(BUTTON_GAP);
        }
        if wide_button(ui, icons, "Custom size…").clicked() {
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
    let width = fields.width.trim().parse::<u32>().ok().filter(|v| *v > 0);
    let height = fields.height.trim().parse::<u32>().ok().filter(|v| *v > 0);
    let max_kb = fields.max_kb.trim().parse::<u64>().ok();
    let ready = width.is_some() && height.is_some() && max_kb.is_some();

    let choice = bottom_bar(ui, "custom-actions", |ui| {
        dialog_buttons(ui, "Resize", ready)
    });

    heading(ui, "Fit the image to:");

    let row = |ui: &mut egui::Ui, caption: &str, value: &mut String| {
        ui.horizontal(|ui| {
            ui.add_sized([180.0, 24.0], egui::Label::new(caption));
            ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY))
        })
        .inner
    };
    let first = row(ui, "Width (px)", &mut fields.width);
    focus_first(ui, &first);
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

    if !ready {
        ui.add_space(10.0);
        ui.colored_label(
            ui.visuals().error_fg_color,
            "Width and height must be above zero, and the size a whole number.",
        );
    }

    match choice.or_else(|| (ready && accepted(ui.ctx())).then_some(Choice::Accept)) {
        Some(Choice::Accept) => match (width, height, max_kb) {
            (Some(width), Some(height), Some(max_kb)) => Some(Reply::ResizeTarget {
                preset: Box::new(Preset {
                    id: "custom".to_string(),
                    label: "Custom".to_string(),
                    width,
                    height,
                    max_bytes: max_kb * 1024,
                    format: Some(fields.format.clone()),
                    fit: if fields.exact {
                        Fit::Exact
                    } else {
                        Fit::Inside
                    },
                    icon: None,
                    button_color: None,
                    text_color: None,
                }),
            }),
            _ => None,
        },
        Some(Choice::Cancel) => Some(Reply::Cancelled),
        None => None,
    }
}

fn ask_video_target(ui: &mut egui::Ui, subject: &str, fields: &mut VideoFields) -> Option<Reply> {
    let target = fields.parse();
    let ready = target.as_ref().is_some_and(|t| !t.constrains_nothing());
    let choice = bottom_bar(ui, "video-actions", |ui| {
        dialog_buttons(ui, "Re-encode", ready)
    });

    heading(ui, &format!("Re-encode {subject} to fit:"));

    let row = |ui: &mut egui::Ui, caption: &str, hint: &str, value: &mut String| {
        ui.horizontal(|ui| {
            ui.add_sized([150.0, 24.0], egui::Label::new(caption));
            ui.add(
                egui::TextEdit::singleline(value)
                    .hint_text(hint)
                    .desired_width(f32::INFINITY),
            )
        })
        .inner
    };
    let width = row(ui, "Width (px)", "leave blank to keep", &mut fields.width);
    focus_first(ui, &width);
    row(ui, "Height (px)", "leave blank to keep", &mut fields.height);
    row(ui, "Max file size", "e.g. 10mb", &mut fields.max_size);
    row(ui, "Length", "e.g. 1:30", &mut fields.length);

    ui.add_space(10.0);
    match target.as_ref() {
        None => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "A size reads like `10mb`, and a length like `90`, `1:30` or `1m30s`.",
            );
        }
        Some(target) if target.constrains_nothing() => {
            ui.label("Fill in at least one; anything left blank is kept as it is.");
        }
        Some(target) => {
            ui.label(format!("Result: {}", target.summary()));
        }
    }

    match choice.or_else(|| (ready && accepted(ui.ctx())).then_some(Choice::Accept)) {
        Some(Choice::Accept) => target.map(|target| Reply::VideoTarget { target }),
        Some(Choice::Cancel) => Some(Reply::Cancelled),
        None => None,
    }
}

fn ask_conversion(
    ui: &mut egui::Ui,
    source: &str,
    options: &[ActionEntry],
    selected: &mut usize,
    icons: &mut Icons,
) -> Option<Reply> {
    heading(ui, &format!("Convert {source} to:"));

    let mut chosen = navigate(ui.ctx(), options.len(), selected)
        .and_then(|index| options.get(index))
        .map(|option| option.id.clone());

    egui::ScrollArea::vertical().show(ui, |ui| {
        for (index, option) in options.iter().enumerate() {
            let mut text = egui::RichText::new(&option.label)
                .size(BUTTON_TEXT)
                .strong();
            if let Some(colour) = color(option.text_color.as_ref()) {
                text = text.color(colour);
            }
            let style = Style::for_action(option).selected(index == *selected);
            if icon_button(ui, icons, &option.id, style, text).clicked() {
                chosen = Some(option.id.clone());
            }
            ui.add_space(BUTTON_GAP);
        }
    });

    chosen.map(|conversion_id| Reply::Conversion { conversion_id })
}

fn ask_replace(
    ui: &mut egui::Ui,
    patterns: &[String],
    replacements: &[String],
    fields: &mut ReplaceFields,
) -> Option<Reply> {
    let problem = clipconv::replace::why_invalid(&fields.pattern);
    let choice = bottom_bar(ui, "replace-actions", |ui| {
        dialog_buttons(ui, "Replace", problem.is_none())
    });

    heading(ui, "Replace every match of:");
    let pattern = history_field(ui, "patterns", &mut fields.pattern, patterns);
    focus_first(ui, &pattern);

    // Only complained about once something has been typed: an empty field on
    // opening is not a mistake yet.
    if let Some(problem) = problem.as_deref().filter(|_| !fields.pattern.is_empty()) {
        ui.colored_label(ui.visuals().error_fg_color, problem);
    }

    ui.add_space(12.0);
    ui.label(
        egui::RichText::new("with, where $1 is the first group:")
            .size(15.0)
            .strong(),
    );
    ui.add_space(6.0);
    history_field(ui, "replacements", &mut fields.replacement, replacements);

    match choice.or_else(|| accepted(ui.ctx()).then_some(Choice::Accept)) {
        Some(Choice::Accept) if problem.is_none() => Some(Reply::Replace {
            replacement: clipconv::replace::Replacement {
                pattern: fields.pattern.clone(),
                replacement: fields.replacement.clone(),
            },
        }),
        Some(Choice::Cancel) => Some(Reply::Cancelled),
        // Accept with a pattern that will not compile is not an answer.
        Some(Choice::Accept) | None => None,
    }
}

fn show_error(ui: &mut egui::Ui, message: &str) -> Option<Reply> {
    let dismissed = bottom_bar(ui, "error-actions", |ui| dismiss_button(ui, "Close"));
    ui.label(egui::RichText::new(message).size(14.0));
    dismissed.then_some(Reply::Acknowledged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions(n: usize) -> Vec<ActionEntry> {
        (0..n)
            .map(|i| ActionEntry {
                id: format!("id{i}"),
                label: format!("Action {i}"),
                icon: None,
                button_color: None,
                text_color: None,
            })
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
            Request::AskReplace {
                patterns: Vec::new(),
                replacements: Vec::new(),
            },
            Request::AskConversion {
                source: "PNG".to_string(),
                options: actions(2),
            },
            Request::AskVideoTarget {
                subject: "1 video".to_string(),
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
                    | (Request::AskReplace { .. }, State::AskReplace { .. })
                    | (Request::AskConversion { .. }, State::AskConversion { .. })
                    | (Request::AskVideoTarget { .. }, State::AskVideoTarget { .. })
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
                State::ChooseAction { paste_after, .. } => assert_eq!(paste_after, remembered),
                _ => panic!("wrong state"),
            }
        }
    }

    #[test]
    fn a_list_dialog_starts_with_the_first_entry_selected() {
        let state = State::for_request(&Request::ChooseAction {
            description: String::new(),
            actions: actions(3),
            paste_after: false,
        });
        match state {
            State::ChooseAction { selected, .. } => assert_eq!(selected, 0),
            _ => panic!("wrong state"),
        }
    }

    #[test]
    fn an_empty_video_form_asks_for_nothing_rather_than_failing_to_parse() {
        let fields = VideoFields::default();
        let target = fields.parse().expect("blank is valid, just empty");
        assert!(target.constrains_nothing());
    }

    #[test]
    fn the_video_form_reads_sizes_and_lengths_as_people_write_them() {
        let fields = VideoFields {
            width: "1280".to_string(),
            height: String::new(),
            max_size: "10mb".to_string(),
            length: "1:30".to_string(),
        };
        let target = fields.parse().expect("all readable");
        assert_eq!(target.width, Some(1280));
        assert_eq!(target.height, None);
        assert_eq!(target.max_bytes, Some(10 * 1024 * 1024));
        assert_eq!(target.seconds, Some(90.0));
    }

    #[test]
    fn a_field_that_does_not_read_as_a_number_is_reported_not_dropped() {
        let fields = VideoFields {
            max_size: "lots".to_string(),
            ..VideoFields::default()
        };
        assert!(fields.parse().is_none());
    }

    #[test]
    fn the_conversion_list_grows_with_the_number_of_options() {
        let few = Request::AskConversion {
            source: "PNG".to_string(),
            options: actions(2),
        };
        let many = Request::AskConversion {
            source: "PNG".to_string(),
            options: actions(6),
        };
        assert!(window_height(&many) > window_height(&few));
        assert!(window_height(&many) <= 720.0);
    }

    #[test]
    fn the_replace_form_starts_from_the_most_recent_history_entry() {
        let state = State::for_request(&Request::AskReplace {
            patterns: vec!["newest".to_string(), "older".to_string()],
            replacements: vec!["$1".to_string()],
        });
        match state {
            State::AskReplace { fields } => {
                assert_eq!(fields.pattern, "newest");
                assert_eq!(fields.replacement, "$1");
            }
            _ => panic!("wrong state"),
        }
    }

    #[test]
    fn an_empty_replace_history_leaves_the_form_blank() {
        let state = State::for_request(&Request::AskReplace {
            patterns: Vec::new(),
            replacements: Vec::new(),
        });
        match state {
            State::AskReplace { fields } => assert!(fields.pattern.is_empty()),
            _ => panic!("wrong state"),
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

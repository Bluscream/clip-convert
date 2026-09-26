//! The resize form: a grid of presets over the limits they fill in.
//!
//! Clicking a preset no longer resizes on the spot. It fills the fields below
//! with that preset's limits and marks the button, so the numbers can be
//! adjusted before committing — and clicking the same preset again applies it
//! straight away, for when no adjustment was wanted. That keeps one click from
//! being an irreversible re-encode while leaving the fast path two clicks away.

use crate::widgets::{
    accepted, bottom_bar, color, dialog_buttons, heading, icon_button, navigate, Choice, Icons,
    Style,
};
use clipconv::cutout::ImageOptions;
use clipconv::encode::Format;
use clipconv::presets::{Fit, Preset};
use clipconv::protocol::Reply;

/// Widest a preset column may be before another column is added.
const COLUMN_WIDTH: f32 = 300.0;
/// Most columns to use, however wide the window is.
const COLUMNS_MAX: usize = 4;
const TITLE: f32 = 15.0;
const SUBTITLE: f32 = 11.5;
const BUTTON_GAP: f32 = 12.0;
const LABEL_WIDTH: f32 = 180.0;

/// The editable limits, which a preset fills in.
pub struct Fields {
    pub width: String,
    pub height: String,
    pub max_kb: String,
    pub format: String,
    pub exact: bool,
}

impl Default for Fields {
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

impl Fields {
    /// Fills the form in from a preset.
    fn take_from(&mut self, preset: &Preset) {
        self.width = preset.width.to_string();
        self.height = preset.height.to_string();
        self.max_kb = (preset.max_bytes / 1024).to_string();
        if let Some(format) = preset.format.as_deref() {
            self.format = format.to_string();
        }
        self.exact = preset.fit == Fit::Exact;
    }

    fn parsed(&self) -> Option<(u32, u32, u64)> {
        let width = self.width.trim().parse::<u32>().ok()?;
        let height = self.height.trim().parse::<u32>().ok()?;
        let max_kb = self.max_kb.trim().parse::<u64>().ok()?;
        Some((width, height, max_kb))
    }
}

/// What the resize form is holding while it is open.
pub struct State {
    pub fields: Fields,
    /// Where the keyboard is.
    pub selected: usize,
    /// The preset whose limits are in the form. Clicking it again applies it.
    pub armed: Option<usize>,
    pub options: ImageOptions,
    /// Whether the checkboxes were changed by hand, which stops a preset from
    /// overwriting them.
    pub options_touched: bool,
}

impl State {
    #[must_use]
    pub fn new(options: ImageOptions) -> Self {
        Self {
            fields: Fields::default(),
            selected: 0,
            armed: None,
            options,
            options_touched: false,
        }
    }
}

/// What a preset should do about transparency and empty space by default.
///
/// Both on, except that a format with no alpha channel cannot hold a removed
/// background — it would be flattened to white on the way out, which is worse
/// than not removing it. Such a preset still crops.
#[must_use]
pub fn defaults_for(preset: &Preset, source_keeps_alpha: bool) -> ImageOptions {
    let keeps_alpha = preset.format.as_deref().map_or(source_keeps_alpha, |name| {
        Format::parse(name).is_none_or(Format::supports_transparency)
    });
    ImageOptions {
        remove_background: keeps_alpha,
        fill_to_borders: true,
    }
}

/// Draws the form and reports a chosen target.
pub fn screen(
    ui: &mut egui::Ui,
    presets: &[Preset],
    source_keeps_alpha: bool,
    state: &mut State,
    icons: &mut Icons,
) -> Option<Reply> {
    heading(ui, "Resize to:");

    let (width, height, max_kb) = state.fields.parsed().unzip3();
    let parses = width.is_some() && height.is_some() && max_kb.is_some();
    // Zero everywhere means no limit at all, which is only work worth doing if
    // one of the checkboxes is.
    let does_something = width.unwrap_or(0) > 0
        || height.unwrap_or(0) > 0
        || max_kb.unwrap_or(0) > 0
        || !state.options.is_noop();
    let ready = parses && does_something;

    let choice = bottom_bar(ui, "resize-actions", |ui| {
        form(ui, state, ready, parses);
        dialog_buttons(ui, "Resize", ready)
    });

    // Applied directly: a second click on an armed preset, which is the
    // shortcut for "these limits, unchanged".
    if let Some(applied) = preset_grid(ui, presets, state, source_keeps_alpha, icons) {
        return Some(Reply::ResizeTarget {
            preset: Box::new(applied),
            options: state.options,
        });
    }

    let accept = choice.or_else(|| (ready && accepted(ui.ctx())).then_some(Choice::Accept));
    match accept {
        Some(Choice::Accept) => {
            let (width, height, max_kb) = (width?, height?, max_kb?);
            // The armed preset's name, so the notification says "for Discord"
            // rather than "for Custom" when its numbers were not touched.
            let (id, label) = state
                .armed
                .and_then(|index| presets.get(index))
                .map_or_else(
                    || ("custom".to_string(), "Custom".to_string()),
                    |preset| (preset.id.clone(), preset.label.clone()),
                );
            Some(Reply::ResizeTarget {
                preset: Box::new(Preset {
                    id,
                    label,
                    width,
                    height,
                    max_bytes: max_kb * 1024,
                    format: Some(state.fields.format.clone()),
                    fit: if state.fields.exact {
                        Fit::Exact
                    } else {
                        Fit::Inside
                    },
                    icon: None,
                    button_color: None,
                    text_color: None,
                }),
                options: state.options,
            })
        }
        Some(Choice::Cancel) => Some(Reply::Cancelled),
        None => None,
    }
}

/// The grid of presets. Returns a preset only when one was applied outright.
fn preset_grid(
    ui: &mut egui::Ui,
    presets: &[Preset],
    state: &mut State,
    source_keeps_alpha: bool,
    icons: &mut Icons,
) -> Option<Preset> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    // A window is never wide enough for this to leave the range.
    let columns = ((ui.available_width() / COLUMN_WIDTH) as usize).clamp(1, COLUMNS_MAX);

    // Moving the keyboard selection arms a preset, exactly as a first click
    // does, so the two ways of driving this behave the same.
    let mut clicked = navigate(ui.ctx(), presets.len(), &mut state.selected);
    let mut applied = None;

    egui::ScrollArea::vertical().show(ui, |ui| {
        for (row_index, row) in presets.chunks(columns).enumerate() {
            ui.columns(columns, |column_uis| {
                for (offset, (column, preset)) in column_uis.iter_mut().zip(row).enumerate() {
                    let index = row_index * columns + offset;
                    let armed = state.armed == Some(index);
                    if preset_button(column, icons, preset, index == state.selected, armed)
                        .clicked()
                    {
                        clicked = Some(index);
                    }
                }
            });
            ui.add_space(BUTTON_GAP);
        }
    });

    if let Some(index) = clicked {
        if let Some(preset) = presets.get(index) {
            if state.armed == Some(index) {
                applied = Some(preset.clone());
            } else {
                state.armed = Some(index);
                state.selected = index;
                state.fields.take_from(preset);
                if !state.options_touched {
                    state.options = defaults_for(preset, source_keeps_alpha);
                }
            }
        }
    }
    applied
}

/// The limits and the two checkboxes, under the grid.
fn form(ui: &mut egui::Ui, state: &mut State, ready: bool, parses: bool) {
    let row = |ui: &mut egui::Ui, caption: &str, value: &mut String| {
        ui.horizontal(|ui| {
            ui.add_sized([LABEL_WIDTH, 24.0], egui::Label::new(caption));
            ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
        });
    };
    row(ui, "Width (px, 0 = any)", &mut state.fields.width);
    row(ui, "Height (px, 0 = any)", &mut state.fields.height);
    row(ui, "Max size (KB, 0 = any)", &mut state.fields.max_kb);

    ui.horizontal(|ui| {
        ui.add_sized([LABEL_WIDTH, 24.0], egui::Label::new("Format"));
        egui::ComboBox::from_id_salt("format")
            .selected_text(state.fields.format.to_uppercase())
            .show_ui(ui, |ui| {
                for name in ["png", "webp", "jpeg", "gif"] {
                    ui.selectable_value(
                        &mut state.fields.format,
                        name.to_string(),
                        name.to_uppercase(),
                    );
                }
            });
        ui.checkbox(&mut state.fields.exact, "Pad to exactly this canvas");
    });

    let before = state.options;
    ui.horizontal(|ui| {
        ui.checkbox(&mut state.options.remove_background, "Remove background")
            .on_hover_text(
                "Detects the colour around the edges and makes it transparent, \
                 leaving anything enclosed by the subject alone.",
            );
        ui.checkbox(&mut state.options.fill_to_borders, "Fill to borders")
            .on_hover_text("Crops every side until the content touches the edge.");
    });
    // Once these are set by hand a preset stops overwriting them: the last
    // thing the user said about them wins.
    if state.options != before {
        state.options_touched = true;
    }

    if !ready {
        let message = if parses {
            "Set a width, a height or a maximum size — or tick one of the boxes."
        } else {
            "Width, height and size must be whole numbers."
        };
        ui.colored_label(ui.visuals().error_fg_color, message);
    }
}

/// A preset's button: its name over its limits, the second line in smaller,
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
    armed: bool,
) -> egui::Response {
    let title =
        color(preset.text_color.as_ref()).unwrap_or_else(|| ui.visuals().strong_text_color());
    let mut text = egui::text::LayoutJob::default();
    text.append(
        &preset.label,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(TITLE),
            color: title,
            ..Default::default()
        },
    );
    // The armed preset says what a second click will do, so the shortcut is
    // discoverable rather than something to be told about. Joined with the
    // same separator as the rest of the line, so it reads as one caption.
    let subtitle = if armed {
        format!("\n{} · click to apply", preset.summary())
    } else {
        format!("\n{}", preset.summary())
    };
    text.append(
        &subtitle,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(SUBTITLE),
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
        Style::for_preset(preset).selected(selected).armed(armed),
        text,
    )
}

/// Splits an `Option` of a triple into a triple of `Option`s, so each field can
/// be reported on separately.
trait Unzip3<A, B, C> {
    fn unzip3(self) -> (Option<A>, Option<B>, Option<C>);
}

impl<A, B, C> Unzip3<A, B, C> for Option<(A, B, C)> {
    fn unzip3(self) -> (Option<A>, Option<B>, Option<C>) {
        match self {
            Some((a, b, c)) => (Some(a), Some(b), Some(c)),
            None => (None, None, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(format: Option<&str>) -> Preset {
        Preset {
            id: "p".to_string(),
            label: "P".to_string(),
            width: 512,
            height: 512,
            max_bytes: 256 * 1024,
            format: format.map(str::to_string),
            fit: Fit::Inside,
            icon: None,
            button_color: None,
            text_color: None,
        }
    }

    #[test]
    fn a_format_with_alpha_removes_the_background_by_default() {
        for format in ["png", "webp", "gif"] {
            let options = defaults_for(&preset(Some(format)), true);
            assert!(options.remove_background, "{format}");
            assert!(options.fill_to_borders, "{format}");
        }
    }

    #[test]
    fn a_format_without_alpha_still_crops_but_keeps_its_background() {
        // Removing it would only be flattened back to white on the way out.
        let options = defaults_for(&preset(Some("jpeg")), true);
        assert!(!options.remove_background);
        assert!(options.fill_to_borders, "cropping still applies");
    }

    #[test]
    fn a_preset_that_keeps_the_source_format_follows_the_source() {
        assert!(defaults_for(&preset(None), true).remove_background);
        assert!(!defaults_for(&preset(None), false).remove_background);
        assert!(defaults_for(&preset(None), false).fill_to_borders);
    }

    #[test]
    fn a_preset_fills_the_form_in() {
        let mut fields = Fields::default();
        let mut source = preset(Some("webp"));
        source.width = 320;
        source.height = 240;
        source.max_bytes = 100 * 1024;
        source.fit = Fit::Exact;

        fields.take_from(&source);
        assert_eq!(fields.width, "320");
        assert_eq!(fields.height, "240");
        assert_eq!(fields.max_kb, "100");
        assert_eq!(fields.format, "webp");
        assert!(fields.exact);
    }

    #[test]
    fn a_preset_that_names_no_format_leaves_the_one_already_chosen() {
        let mut fields = Fields {
            format: "jpeg".to_string(),
            ..Fields::default()
        };
        fields.take_from(&preset(None));
        assert_eq!(fields.format, "jpeg");
    }

    #[test]
    fn zeroes_parse_and_mean_no_limit() {
        let fields = Fields {
            width: "0".to_string(),
            height: "0".to_string(),
            max_kb: "0".to_string(),
            format: "png".to_string(),
            exact: false,
        };
        assert_eq!(fields.parsed(), Some((0, 0, 0)));
    }

    #[test]
    fn text_where_a_number_belongs_does_not_parse() {
        let fields = Fields {
            width: "wide".to_string(),
            ..Fields::default()
        };
        assert_eq!(fields.parsed(), None);
    }
}

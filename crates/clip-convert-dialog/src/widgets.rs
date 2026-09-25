//! The pieces every dialog is drawn from.
//!
//! Buttons here are painted by hand rather than composed from
//! `Button::image_and_text`, which aligns its contents to the left edge.
//! Centring an icon and a label as one group is what keeps a menu of mixed
//! entries — some with icons, some without — looking like one list, and
//! painting directly is also what lets an entry carry its own colour without
//! losing the hover and press states.

use clipconv::presets::Preset;
use clipconv::protocol::ActionEntry;
use std::collections::HashMap;

/// Edge length of a button's icon, in logical points.
const ICON_SIZE: f32 = 28.0;
/// Gap between an icon and the label beside it.
const ICON_GAP: f32 = 12.0;
/// Breathing room inside a button, either side of its contents.
const BUTTON_PADDING: f32 = 14.0;
/// Minimum height of an action button. Deliberately large: these are the
/// primary targets, hit immediately after a hotkey, often without looking
/// closely. A button grows past this when its label needs two lines.
pub const BUTTON_HEIGHT: f32 = 58.0;
/// Size of the text on an action button.
pub const BUTTON_TEXT: f32 = 18.0;

/// Decoded icons, kept for the life of the dialog.
///
/// A texture must be uploaded once and reused: decoding and uploading on every
/// frame would make redrawing far more expensive than it needs to be.
#[derive(Default)]
pub struct Icons {
    textures: HashMap<String, Option<egui::TextureHandle>>,
}

impl Icons {
    /// The texture for a base64 icon, decoding it the first time it is needed.
    ///
    /// A failure is cached too, so a broken icon is not retried every frame.
    pub fn get(
        &mut self,
        ctx: &egui::Context,
        key: &str,
        encoded: &str,
    ) -> Option<egui::TextureHandle> {
        if let Some(cached) = self.textures.get(key) {
            return cached.clone();
        }

        let handle = clipconv::icons::decode(encoded)
            .and_then(|bytes| image::load_from_memory(&bytes).ok())
            .map(|decoded| {
                let rgba = decoded.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                ctx.load_texture(key, image, egui::TextureOptions::LINEAR)
            });

        if handle.is_none() {
            log::warn!("could not decode the icon for `{key}`");
        }
        self.textures.insert(key.to_string(), handle.clone());
        handle
    }
}

/// A configured colour, if it was set and understood.
///
/// Anything unparseable was already rejected when the config loaded, so a
/// `None` here means the entry simply did not set one.
pub fn color(raw: Option<&String>) -> Option<egui::Color32> {
    let [r, g, b, a] = clipconv::color::parse(raw?)?;
    Some(egui::Color32::from_rgba_unmultiplied(r, g, b, a))
}

/// A custom fill, adjusted so it still reacts to the pointer.
///
/// A flat colour painted regardless of state loses the hover and press
/// feedback every other control has, which makes a coloured button feel dead.
fn interactive(fill: egui::Color32, response: &egui::Response) -> egui::Color32 {
    if response.is_pointer_button_down_on() {
        fill.gamma_multiply(0.8)
    } else if response.hovered() {
        fill.gamma_multiply(1.2)
    } else {
        fill
    }
}

/// A button with an optional icon to the left of its text, the pair centred.
///
/// Drawn rather than composed from `Button::image_and_text`, which aligns its
/// contents to the left edge. Centring the icon and label as one group is what
/// keeps a menu of mixed entries — some with icons, some without — looking like
/// one list, and painting it directly is also what lets an entry carry its own
/// colour without losing the hover and press states.
pub fn icon_button(
    ui: &mut egui::Ui,
    icons: &mut Icons,
    key: &str,
    style: Style<'_>,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    let texture = style
        .icon
        .and_then(|encoded| icons.get(ui.ctx(), key, encoded));
    let width = ui.available_width();

    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, BUTTON_HEIGHT), egui::Sense::click());

    // The label has to fit in what is left once the icon and its gap are taken.
    let icon_space = if texture.is_some() {
        ICON_SIZE + ICON_GAP
    } else {
        0.0
    };
    let text_limit = (width - icon_space - BUTTON_PADDING * 2.0).max(1.0);
    let galley = text.into().into_galley(
        ui,
        Some(egui::TextWrapMode::Wrap),
        text_limit,
        egui::TextStyle::Button,
    );

    if ui.is_rect_visible(rect) {
        // Painted with the same visuals a real button would use, so it responds
        // to hover and focus like every other control.
        let visuals = ui.style().interact(&response);
        let fill = color(style.button_color)
            .map_or(visuals.weak_bg_fill, |fill| interactive(fill, &response));
        ui.painter().rect(
            rect.expand(visuals.expansion),
            visuals.rounding,
            fill,
            visuals.bg_stroke,
        );

        let group_width = icon_space + galley.size().x;
        let left = rect.center().x - group_width / 2.0;

        if let Some(texture) = texture {
            let icon_rect = egui::Rect::from_min_size(
                egui::pos2(left, rect.center().y - ICON_SIZE / 2.0),
                egui::vec2(ICON_SIZE, ICON_SIZE),
            );
            // White tint means "draw it as it is"; egui would otherwise recolour
            // the icon to match the label and flatten a colourful one.
            egui::Image::from_texture(egui::load::SizedTexture::from_handle(&texture))
                .tint(egui::Color32::WHITE)
                .paint_at(ui, icon_rect);
        }

        let text_pos = egui::pos2(left + icon_space, rect.center().y - galley.size().y / 2.0);
        // A galley that set no colour of its own is painted in this one, so a
        // configured text colour reaches a plain label without the caller
        // having to build it differently.
        let text_color = color(style.text_color).unwrap_or_else(|| visuals.text_color());
        ui.painter().galley(text_pos, galley, text_color);
    }

    response
}

/// What an entry looks like: its icon and its own colours, if it set any.
#[derive(Clone, Copy, Default)]
pub struct Style<'a> {
    pub icon: Option<&'a str>,
    pub button_color: Option<&'a String>,
    pub text_color: Option<&'a String>,
}

impl<'a> Style<'a> {
    pub fn for_action(action: &'a ActionEntry) -> Self {
        Self {
            icon: action.icon.as_deref(),
            button_color: action.button_color.as_ref(),
            text_color: action.text_color.as_ref(),
        }
    }

    pub fn for_preset(preset: &'a Preset) -> Self {
        Self {
            icon: preset.icon.as_deref(),
            button_color: preset.button_color.as_ref(),
            text_color: preset.text_color.as_ref(),
        }
    }
}

/// A full-width button sized for quick, confident clicking.
///
/// `add_sized` rather than `min_size`, because only the former centres the
/// label; a button given a minimum size draws its text against the left edge.
/// Wrapping is on so a long label from a custom action folds onto a second line
/// instead of running off the edge.
pub fn wide_button(ui: &mut egui::Ui, icons: &mut Icons, label: &str) -> egui::Response {
    let text = egui::RichText::new(label).size(BUTTON_TEXT).strong();
    icon_button(ui, icons, label, Style::default(), text)
}

/// A bold line summarising what is on the clipboard.
pub fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(15.0).strong());
    ui.add_space(14.0);
}

/// Width of the "Recent" picker beside a history field.
const PICKER_WIDTH: f32 = 96.0;

/// A text field with the values used before offered beside it.
///
/// A plain dropdown cannot take a new value and a plain field cannot offer an
/// old one, so this is both: type anything, or pick something typed before.
/// The picker is disabled rather than hidden when there is no history, so the
/// field does not change width the first time the action is used.
pub fn history_field(ui: &mut egui::Ui, id: &str, value: &mut String, history: &[String]) {
    ui.horizontal(|ui| {
        let field_width =
            (ui.available_width() - PICKER_WIDTH - ui.spacing().item_spacing.x).max(PICKER_WIDTH);
        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(field_width)
                .font(egui::TextStyle::Monospace),
        );
        ui.add_enabled_ui(!history.is_empty(), |ui| {
            egui::ComboBox::from_id_salt(id)
                .width(PICKER_WIDTH)
                .selected_text("Recent")
                .show_ui(ui, |ui| {
                    for past in history {
                        if ui.selectable_label(past == value, past).clicked() {
                            value.clone_from(past);
                        }
                    }
                });
        });
    });
}

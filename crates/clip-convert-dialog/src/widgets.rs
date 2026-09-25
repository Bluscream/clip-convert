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
        // The keyboard selection is drawn as the same outline focus uses, so
        // there is one visual language for "this is where you are".
        let stroke = if style.selected {
            ui.visuals().selection.stroke
        } else {
            visuals.bg_stroke
        };
        ui.painter().rect(
            rect.expand(visuals.expansion),
            visuals.rounding,
            fill,
            stroke,
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
    /// Whether the keyboard selection is on this entry.
    pub selected: bool,
}

impl<'a> Style<'a> {
    pub fn for_action(action: &'a ActionEntry) -> Self {
        Self {
            icon: action.icon.as_deref(),
            button_color: action.button_color.as_ref(),
            text_color: action.text_color.as_ref(),
            selected: false,
        }
    }

    pub fn for_preset(preset: &'a Preset) -> Self {
        Self {
            icon: preset.icon.as_deref(),
            button_color: preset.button_color.as_ref(),
            text_color: preset.text_color.as_ref(),
            selected: false,
        }
    }

    /// The same style, marked as the keyboard's current selection.
    #[must_use]
    pub fn selected(self, selected: bool) -> Self {
        Self { selected, ..self }
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

/// Width of the arrow that opens a history field's list.
const ARROW_WIDTH: f32 = 28.0;

/// Height of a single-line text control and the arrow beside it.
const FIELD_HEIGHT: f32 = 26.0;

/// Edge length of the corner close button.
const CLOSE_SIZE: f32 = 22.0;

/// Height of a dialog's action buttons.
const ACTION_HEIGHT: f32 = 38.0;

/// A text field with an arrow on its end that drops down what was used before.
///
/// A plain dropdown cannot take a new value and a plain field cannot offer an
/// old one, so this is both — and drawn flush, with no spacing between them, so
/// it reads as one control rather than a field that happens to have a list
/// beside it. The list is as wide as the field it fills in.
///
/// The arrow is disabled rather than hidden when there is no history, so the
/// field does not change width the first time the action is used.
pub fn history_field(ui: &mut egui::Ui, id: &str, value: &mut String, history: &[String]) {
    let popup_id = ui.make_persistent_id(id);
    let total = ui.available_width();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;

        let field = ui.add_sized(
            [(total - ARROW_WIDTH).max(ARROW_WIDTH), FIELD_HEIGHT],
            egui::TextEdit::singleline(value).font(egui::TextStyle::Monospace),
        );
        if drop_arrow(ui, !history.is_empty()).clicked() {
            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        }

        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &field,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(total);
                for past in history {
                    if ui.selectable_label(past == value, past).clicked() {
                        value.clone_from(past);
                    }
                }
            },
        );
    });
}

/// The arrow on the end of a history field, painted rather than written.
///
/// The bundled fonts have no glyph for a small solid triangle — it comes out
/// as an empty box — so the shape is drawn directly, which also makes it
/// independent of whatever fonts a system happens to have.
fn drop_arrow(ui: &mut egui::Ui, enabled: bool) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ARROW_WIDTH, FIELD_HEIGHT), sense);

    if ui.is_rect_visible(rect) {
        let visuals = if enabled {
            *ui.style().interact(&response)
        } else {
            ui.visuals().widgets.noninteractive
        };
        ui.painter().rect(
            rect,
            visuals.rounding,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
        );

        let centre = rect.center();
        let half = 4.0;
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(centre.x - half, centre.y - half / 2.0),
                egui::pos2(centre.x + half, centre.y - half / 2.0),
                egui::pos2(centre.x, centre.y + half),
            ],
            visuals.fg_stroke.color,
            egui::Stroke::NONE,
        ));
    }
    response
}

/// What a dialog's button row was clicked with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Accept,
    Cancel,
}

/// Runs `add` in a strip pinned to the bottom edge of the dialog.
///
/// A window stretched tall should not leave its actions stranded in the middle
/// of an empty panel. Pinning them to the bottom is what makes a resized window
/// look deliberate rather than half-filled.
pub fn bottom_bar<R>(
    ui: &mut egui::Ui,
    id: &'static str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::TopBottomPanel::bottom(id)
        .frame(egui::Frame::none().outer_margin(egui::Margin {
            top: 12.0,
            ..egui::Margin::ZERO
        }))
        .show_inside(ui, add)
        .inner
}

/// The usual pair of dialog buttons, sharing the full width between them.
///
/// `add_sized` rather than `min_size`: only the former centres the label. A
/// button merely given a minimum size draws its text against the left edge,
/// which looks like a mistake once the button is half a window wide.
pub fn dialog_buttons(ui: &mut egui::Ui, accept: &str, ready: bool) -> Option<Choice> {
    let mut chosen = None;
    ui.columns(2, |columns| {
        let width = columns[0].available_width();
        let accept = egui::Button::new(egui::RichText::new(accept).size(15.0).strong());
        if columns[0]
            .add_enabled_ui(ready, |ui| {
                ui.add_sized([width, ACTION_HEIGHT], accept).clicked()
            })
            .inner
        {
            chosen = Some(Choice::Accept);
        }

        let width = columns[1].available_width();
        let cancel = egui::Button::new(egui::RichText::new("Cancel").size(15.0));
        if columns[1]
            .add_sized([width, ACTION_HEIGHT], cancel)
            .clicked()
        {
            chosen = Some(Choice::Cancel);
        }
    });
    chosen
}

/// One button filling the width, for a dialog with nothing to decide.
pub fn dismiss_button(ui: &mut egui::Ui, label: &str) -> bool {
    let width = ui.available_width();
    ui.add_sized(
        [width, ACTION_HEIGHT],
        egui::Button::new(egui::RichText::new(label).size(15.0).strong()),
    )
    .clicked()
}

/// A frameless ✕ in the top-right corner of the window.
///
/// A compositor is not obliged to decorate a window, and a dialog with no
/// title bar and no way to close it is a trap — Escape works, but nothing on
/// screen says so. Drawn without a background so it reads as part of the
/// chrome rather than as one of the dialog's own choices.
pub fn close_button(ctx: &egui::Context) -> bool {
    egui::Area::new(egui::Id::new("close-button"))
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-10.0, 8.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            // Painted rather than written, for the same reason as the arrow:
            // the bundled fonts have no cross, and a missing glyph in the one
            // control that closes the window is not a risk worth taking.
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(CLOSE_SIZE, CLOSE_SIZE), egui::Sense::click());

            if ui.is_rect_visible(rect) {
                let colour = if response.hovered() {
                    ui.visuals().strong_text_color()
                } else {
                    ui.visuals().weak_text_color()
                };
                let arm = rect.shrink(CLOSE_SIZE * 0.3);
                let stroke = egui::Stroke::new(1.6_f32, colour);
                ui.painter()
                    .line_segment([arm.left_top(), arm.right_bottom()], stroke);
                ui.painter()
                    .line_segment([arm.right_top(), arm.left_bottom()], stroke);
            }
            response.on_hover_text("Close").clicked()
        })
        .inner
}

/// Moves a selection with the keyboard, and says when one was chosen.
///
/// These dialogs open under a hotkey: the hands that pressed it are already on
/// the keyboard, and reaching for the mouse to pick from a list of four is the
/// slow way round. Arrows or Tab move, Enter or Space chooses, and a digit
/// picks that entry outright.
///
/// Returns the index that was activated, if any.
pub fn navigate(ctx: &egui::Context, len: usize, selected: &mut usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    *selected = (*selected).min(len - 1);
    let mut activated = None;

    ctx.input(|input| {
        let forward = input.key_pressed(egui::Key::ArrowDown)
            || input.key_pressed(egui::Key::ArrowRight)
            || (input.key_pressed(egui::Key::Tab) && !input.modifiers.shift);
        let backward = input.key_pressed(egui::Key::ArrowUp)
            || input.key_pressed(egui::Key::ArrowLeft)
            || (input.key_pressed(egui::Key::Tab) && input.modifiers.shift);

        if forward {
            *selected = (*selected + 1) % len;
        }
        if backward {
            *selected = (*selected + len - 1) % len;
        }
        if input.key_pressed(egui::Key::Home) {
            *selected = 0;
        }
        if input.key_pressed(egui::Key::End) {
            *selected = len - 1;
        }
        if input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space) {
            activated = Some(*selected);
        }

        // A digit picks that entry directly, which is the fastest path of all
        // for a menu someone uses every day.
        for (offset, key) in DIGITS.iter().enumerate() {
            if input.key_pressed(*key) && offset < len {
                *selected = offset;
                activated = Some(offset);
            }
        }
    });

    activated
}

/// The digit keys, in order, for picking an entry by number.
const DIGITS: [egui::Key; 9] = [
    egui::Key::Num1,
    egui::Key::Num2,
    egui::Key::Num3,
    egui::Key::Num4,
    egui::Key::Num5,
    egui::Key::Num6,
    egui::Key::Num7,
    egui::Key::Num8,
    egui::Key::Num9,
];

/// Whether Enter was pressed this frame.
///
/// Used by the forms instead of a text field's `lost_focus`, which does not
/// reliably report Enter and left these dialogs unsubmittable from the
/// keyboard. Nothing in a dialog with no multi-line field wants Enter for
/// anything else.
pub fn accepted(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.key_pressed(egui::Key::Enter))
}

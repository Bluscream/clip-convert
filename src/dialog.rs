//! The dialogs, drawn with GTK.
//!
//! Every function here must be called on the GTK main thread. They each spin a
//! nested main loop and return the user's choice, so the calling code reads
//! top-to-bottom; the worker thread that wants an answer goes through
//! [`crate::ui`], which marshals the request here.

use gtk::prelude::*;
use gtk::{Align, Orientation, ResponseType};
use lcc_core::presets::{Fit, Preset};

/// Width of every dialog. Wide enough for a long action label and a content
/// summary without wrapping.
const DIALOG_WIDTH: i32 = 460;

/// Styling for the action buttons.
///
/// Applied as CSS rather than through widget properties so the buttons keep the
/// user's theme colours and only their size and weight are overridden.
const CSS: &str = "
.lcc-action {
    min-height: 54px;
    padding: 14px 24px;
    font-size: 16px;
    font-weight: 700;
}
.lcc-heading {
    font-size: 15px;
    font-weight: 700;
}
.lcc-sub {
    font-size: 12px;
    opacity: 0.65;
}
";

/// What GTK currently believes about dark mode.
///
/// This is what a `gtk-application-prefer-dark-theme` line in `settings.ini`
/// sets, and serves as the fallback when the portal cannot be reached.
#[must_use]
pub fn gtk_prefers_dark() -> bool {
    gtk::Settings::default().is_some_and(|s| s.is_gtk_application_prefer_dark_theme())
}

/// Applies the session's light/dark preference.
///
/// GTK 3 does not read the desktop portal's `color-scheme`, so on a KDE session
/// it would otherwise draw a light dialog in a dark desktop. The value is taken
/// from GTK's own settings where it is set, which is what the desktop's control
/// panel writes.
pub fn apply_color_scheme(prefer_dark: bool) {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(prefer_dark);
        log::debug!("dark theme preferred: {prefer_dark}");
    }
}

/// Installs the stylesheet once for the whole process.
pub fn install_css() {
    let provider = gtk::CssProvider::new();
    if let Err(e) = provider.load_from_data(CSS.as_bytes()) {
        log::warn!("could not load dialog stylesheet: {e}");
        return;
    }
    if let Some(screen) = gtk::gdk::Screen::default() {
        gtk::StyleContext::add_provider_for_screen(
            &screen,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// Builds a dialog that floats above the focused window and is centred.
fn shell(title: &str) -> gtk::Dialog {
    let dialog = gtk::Dialog::builder()
        .title(title)
        .modal(true)
        .resizable(false)
        .default_width(DIALOG_WIDTH)
        .window_position(gtk::WindowPosition::CenterAlways)
        .build();
    dialog.set_keep_above(true);
    dialog
}

/// Adds generous padding around a dialog's content.
fn content_box(dialog: &gtk::Dialog) -> gtk::Box {
    let area = dialog.content_area();
    let holder = gtk::Box::new(Orientation::Vertical, 12);
    holder.set_margin_top(22);
    holder.set_margin_bottom(18);
    holder.set_margin_start(22);
    holder.set_margin_end(22);
    area.add(&holder);
    holder
}

/// Adds a bold summary line describing what is on the clipboard.
fn add_heading(holder: &gtk::Box, text: &str) {
    let label = gtk::Label::new(Some(text));
    label.set_halign(Align::Start);
    label.set_xalign(0.0);
    label.style_context().add_class("lcc-heading");
    label.set_margin_bottom(6);
    holder.add(&label);
}

/// What the action dialog came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionChoice {
    /// The `id` of the chosen action.
    pub action_id: String,
    /// The state of the checkbox when the choice was made.
    pub paste_after: bool,
}

/// Offers `actions` as a column of wide buttons under a summary of the content.
///
/// `actions` is a list of `(id, label)`. Returns `None` if the user dismissed
/// the dialog.
#[must_use]
pub fn choose_action(
    description: &str,
    actions: &[(String, String)],
    paste_after: bool,
) -> Option<ActionChoice> {
    let dialog = shell("Clipboard actions");
    let holder = content_box(&dialog);
    add_heading(&holder, description);

    // Each button answers with its own response code, so the click itself
    // carries the choice and no selection state has to be tracked.
    let buttons = gtk::Box::new(Orientation::Vertical, 12);
    for (index, (_, label)) in actions.iter().enumerate() {
        let button = gtk::Button::with_label(label);
        button.style_context().add_class("lcc-action");
        button.set_hexpand(true);
        let dialog_ref = dialog.clone();
        let code = i32::try_from(index).unwrap_or(i32::MAX);
        button.connect_clicked(move |_| {
            dialog_ref.response(ResponseType::Other(u16::try_from(code).unwrap_or(u16::MAX)));
        });
        buttons.add(&button);
    }
    holder.add(&buttons);

    let check = gtk::CheckButton::with_label("Paste after action");
    check.set_active(paste_after);
    check.set_margin_top(14);
    holder.add(&check);

    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.show_all();

    let response = dialog.run();
    let paste_after = check.is_active();
    let chosen = match response {
        ResponseType::Other(index) => actions.get(usize::from(index)).map(|(id, _)| ActionChoice {
            action_id: id.clone(),
            paste_after,
        }),
        _ => None,
    };
    // Always report the checkbox back so the setting is remembered even when the
    // dialog was cancelled.
    dialog.close();
    chosen
}

/// Asks for a character limit.
///
/// Returns `None` if dismissed.
#[must_use]
pub fn ask_limit(title: &str, message: &str, default: usize) -> Option<usize> {
    let dialog = shell(title);
    let holder = content_box(&dialog);
    add_heading(&holder, message);

    let entry = gtk::SpinButton::with_range(1.0, 1_000_000.0, 1.0);
    #[allow(clippy::cast_precision_loss)]
    // A limit large enough to lose precision is not meaningful.
    entry.set_value(default as f64);
    entry.set_numeric(true);
    entry.set_activates_default(true);
    holder.add(&entry);

    dialog.add_button("Cancel", ResponseType::Cancel);
    let ok = dialog.add_button("Continue", ResponseType::Accept);
    ok.style_context().add_class("suggested-action");
    dialog.set_default_response(ResponseType::Accept);
    dialog.show_all();
    entry.grab_focus();

    let response = dialog.run();
    let value = entry.value_as_int();
    dialog.close();

    if response == ResponseType::Accept {
        usize::try_from(value).ok().filter(|v| *v > 0)
    } else {
        None
    }
}

/// Offers the configured size presets, plus a custom option.
///
/// Returns `None` if dismissed.
#[must_use]
pub fn ask_resize_target(presets: &[Preset]) -> Option<Preset> {
    let dialog = shell("Resize image");
    let holder = content_box(&dialog);
    add_heading(&holder, "Resize to:");

    let buttons = gtk::Box::new(Orientation::Vertical, 12);
    for (index, preset) in presets.iter().enumerate() {
        // A two-line button: the name, and the limits it encodes, so the choice
        // does not require remembering each platform's rules.
        let inner = gtk::Box::new(Orientation::Vertical, 2);
        let name = gtk::Label::new(Some(&preset.label));
        name.set_xalign(0.0);
        let detail = gtk::Label::new(Some(&preset.summary()));
        detail.set_xalign(0.0);
        detail.style_context().add_class("lcc-sub");
        inner.add(&name);
        inner.add(&detail);

        let button = gtk::Button::new();
        button.add(&inner);
        button.style_context().add_class("lcc-action");
        button.set_hexpand(true);

        let dialog_ref = dialog.clone();
        let code = u16::try_from(index).unwrap_or(u16::MAX);
        button.connect_clicked(move |_| dialog_ref.response(ResponseType::Other(code)));
        buttons.add(&button);
    }
    holder.add(&buttons);

    let custom = gtk::Button::with_label("Custom size…");
    custom.style_context().add_class("lcc-action");
    custom.set_margin_top(4);
    let dialog_ref = dialog.clone();
    custom.connect_clicked(move |_| dialog_ref.response(ResponseType::Apply));
    holder.add(&custom);

    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.show_all();

    let response = dialog.run();
    dialog.close();

    match response {
        ResponseType::Other(index) => presets.get(usize::from(index)).cloned(),
        ResponseType::Apply => ask_custom_size(),
        _ => None,
    }
}

/// Asks for explicit dimensions, a size cap and a format.
fn ask_custom_size() -> Option<Preset> {
    let dialog = shell("Custom size");
    let holder = content_box(&dialog);
    add_heading(&holder, "Fit the image to:");

    let grid = gtk::Grid::new();
    grid.set_row_spacing(8);
    grid.set_column_spacing(12);

    let field = |grid: &gtk::Grid, row: i32, caption: &str, max: f64, value: f64| {
        let label = gtk::Label::new(Some(caption));
        label.set_xalign(0.0);
        let spin = gtk::SpinButton::with_range(0.0, max, 1.0);
        spin.set_value(value);
        spin.set_numeric(true);
        spin.set_hexpand(true);
        grid.attach(&label, 0, row, 1, 1);
        grid.attach(&spin, 1, row, 1, 1);
        spin
    };

    let width = field(&grid, 0, "Width (px)", 20_000.0, 512.0);
    let height = field(&grid, 1, "Height (px)", 20_000.0, 512.0);
    let max_kb = field(&grid, 2, "Max size (KB, 0 = any)", 1_000_000.0, 0.0);

    let format_label = gtk::Label::new(Some("Format"));
    format_label.set_xalign(0.0);
    let format = gtk::ComboBoxText::new();
    for name in ["png", "webp", "jpeg", "gif"] {
        format.append_text(name);
    }
    format.set_active(Some(0));
    grid.attach(&format_label, 0, 3, 1, 1);
    grid.attach(&format, 1, 3, 1, 1);
    holder.add(&grid);

    let exact = gtk::CheckButton::with_label("Pad to exactly this canvas");
    exact.set_margin_top(8);
    holder.add(&exact);

    dialog.add_button("Cancel", ResponseType::Cancel);
    let ok = dialog.add_button("Resize", ResponseType::Accept);
    ok.style_context().add_class("suggested-action");
    dialog.set_default_response(ResponseType::Accept);
    dialog.show_all();

    let response = dialog.run();
    let chosen = Preset {
        id: "custom".to_string(),
        label: "Custom".to_string(),
        width: u32::try_from(width.value_as_int()).unwrap_or(512).max(1),
        height: u32::try_from(height.value_as_int()).unwrap_or(512).max(1),
        max_bytes: u64::try_from(max_kb.value_as_int()).unwrap_or(0) * 1024,
        format: format
            .active_text()
            .map_or_else(|| "png".to_string(), |t| t.to_string()),
        fit: if exact.is_active() {
            Fit::Exact
        } else {
            Fit::Inside
        },
    };
    dialog.close();

    (response == ResponseType::Accept).then_some(chosen)
}

/// Shows an error the user needs to see, rather than only logging it.
pub fn show_error(message: &str) {
    let dialog = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Error,
        gtk::ButtonsType::Close,
        message,
    );
    dialog.set_keep_above(true);
    dialog.run();
    dialog.close();
}

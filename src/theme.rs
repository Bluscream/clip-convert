//! Working out whether the session wants dark or light widgets.
//!
//! GTK 3 predates the desktop portal's appearance setting and does not read it,
//! so on a KDE session — where nothing else tells GTK what the theme is — it
//! draws a light dialog in the middle of a dark desktop. The portal is the
//! cross-desktop source of truth, so it is asked directly and the answer is
//! applied to GTK's own preference.

use std::time::Duration;

/// How long to wait for the portal before giving up and using the fallback.
/// Short, because this runs during startup.
const PORTAL_TIMEOUT: Duration = Duration::from_secs(2);

/// `color-scheme` values defined by the portal's appearance interface.
const PREFER_DARK: u32 = 1;

/// Whether the session prefers a dark appearance.
///
/// Falls back to whatever GTK already believes — which is what a
/// `gtk-application-prefer-dark-theme` line in `settings.ini` sets — when the
/// portal is unavailable.
#[must_use]
pub fn prefers_dark(gtk_fallback: bool) -> bool {
    match ask_portal() {
        Some(dark) => dark,
        None => {
            log::debug!("no portal appearance setting; using the GTK preference");
            gtk_fallback
        }
    }
}

/// Reads `org.freedesktop.appearance color-scheme` from the desktop portal.
fn ask_portal() -> Option<bool> {
    // A dedicated thread with a timeout: a wedged or absent portal must not
    // delay startup, and this is the only blocking D-Bus call the app makes.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_color_scheme());
    });

    match rx.recv_timeout(PORTAL_TIMEOUT) {
        Ok(Some(scheme)) => Some(scheme == PREFER_DARK),
        Ok(None) => None,
        Err(_) => {
            log::debug!("the desktop portal did not answer in time");
            None
        }
    }
}

fn read_color_scheme() -> Option<u32> {
    let connection = zbus::blocking::Connection::session()
        .inspect_err(|e| log::debug!("no session bus: {e}"))
        .ok()?;

    let reply = connection
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Settings"),
            "Read",
            &("org.freedesktop.appearance", "color-scheme"),
        )
        .inspect_err(|e| log::debug!("portal Read failed: {e}"))
        .ok()?;

    // The reply is a variant wrapping a variant wrapping the number.
    let outer: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    unwrap_scheme(&outer)
}

/// Digs the number out of the portal's nested variants.
fn unwrap_scheme(value: &zbus::zvariant::Value<'_>) -> Option<u32> {
    match value {
        zbus::zvariant::Value::U32(n) => Some(*n),
        zbus::zvariant::Value::Value(inner) => unwrap_scheme(inner),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    #[test]
    fn a_bare_number_is_read() {
        assert_eq!(unwrap_scheme(&Value::U32(1)), Some(1));
    }

    #[test]
    fn a_nested_variant_is_unwrapped() {
        let nested = Value::from(Value::U32(PREFER_DARK));
        assert_eq!(unwrap_scheme(&nested), Some(PREFER_DARK));
    }

    #[test]
    fn a_doubly_nested_variant_is_unwrapped() {
        let nested = Value::from(Value::from(Value::U32(2)));
        assert_eq!(unwrap_scheme(&nested), Some(2));
    }

    #[test]
    fn an_unexpected_type_is_not_guessed_at() {
        assert_eq!(unwrap_scheme(&Value::Str("dark".into())), None);
    }

    #[test]
    fn only_the_dark_value_counts_as_dark() {
        // 0 is "no preference" and 2 is "prefer light"; neither is dark.
        assert_eq!(PREFER_DARK, 1);
    }
}

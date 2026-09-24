//! Deciding whether a freshly copied URL should be shortened automatically.
//!
//! Kept separate from the act of shortening because this is the part with all
//! the edge cases, and every one of them is a rule a user can observe: the most
//! important is that the app must never shorten its own output, which would
//! otherwise loop forever as each write triggers the next clipboard change.

use crate::config::{Config, Shortener};
use url::Url;

/// Why a copied URL was left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Auto-shortening is switched off in the tray.
    Disabled,
    /// No usable backend is configured.
    NoBackends,
    /// The user held the bypass key while copying.
    BypassHeld,
    /// Scroll Lock is on, which disables auto-shortening entirely.
    ScrollLock,
    /// The URL matches `blacklist_regex`.
    Blacklisted,
    /// It is already a link from one of the configured shorteners.
    AlreadyShort,
    /// It is exactly what this app last put on the clipboard.
    OwnOutput,
    /// The same URL was copied twice in a row, which means "leave it alone".
    CopiedTwice,
}

impl Skip {
    /// A short explanation, for the debug log.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Disabled => "auto-shortening is disabled",
            Self::NoBackends => "no shortener is configured",
            Self::BypassHeld => "the bypass key was held",
            Self::ScrollLock => "Scroll Lock is on",
            Self::Blacklisted => "the URL matches blacklist_regex",
            Self::AlreadyShort => "the URL is already a shortened link",
            Self::OwnOutput => "the URL is this app's own output",
            Self::CopiedTwice => "the same URL was copied twice in a row",
        }
    }
}

/// Live state the decision depends on that cannot be read from the config.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context<'a> {
    /// The short URL this app most recently wrote to the clipboard.
    pub last_written: Option<&'a str>,
    /// The URL that was most recently considered for shortening.
    pub last_seen: Option<&'a str>,
    /// Whether the bypass modifier was held at the moment of copying.
    pub bypass_held: bool,
    /// Whether Scroll Lock is currently on.
    pub scroll_lock: bool,
}

/// Decides whether `url` should be shortened now.
///
/// # Errors
///
/// Returns the [`Skip`] reason when it should be left alone. This is an
/// ordinary outcome, not a failure.
pub fn decide(
    url: &Url,
    config: &Config,
    shorteners: &[&Shortener],
    context: Context<'_>,
) -> Result<(), Skip> {
    if !config.auto_shorten {
        return Err(Skip::Disabled);
    }
    if shorteners.is_empty() {
        return Err(Skip::NoBackends);
    }
    if config.bypass_shift && context.bypass_held {
        return Err(Skip::BypassHeld);
    }
    if config.bypass_scroll_lock && context.scroll_lock {
        return Err(Skip::ScrollLock);
    }

    // Checked before anything else about the URL's provenance so that the app
    // can never be made to shorten a link it just produced.
    if context.last_written == Some(url.as_str()) {
        return Err(Skip::OwnOutput);
    }
    if crate::shorten::is_already_short(url, shorteners) {
        return Err(Skip::AlreadyShort);
    }

    if config.bypass_double_copy && context.last_seen == Some(url.as_str()) {
        return Err(Skip::CopiedTwice);
    }

    if !config.blacklist_regex.trim().is_empty() {
        match regex::Regex::new(&config.blacklist_regex) {
            Ok(pattern) if pattern.is_match(url.as_str()) => return Err(Skip::Blacklisted),
            Ok(_) => {}
            // Validated at load; a broken pattern here must not block every URL.
            Err(e) => log::warn!("ignoring invalid blacklist_regex: {e}"),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ShortenerKind;

    fn backend() -> Shortener {
        Shortener {
            name: "s".to_string(),
            kind: ShortenerKind::Yourls,
            api_url: "https://s.example/yourls-api.php".to_string(),
            signature: "secret".to_string(),
            base_url: "https://s.example/".to_string(),
            command: Vec::new(),
            enabled: true,
        }
    }

    fn url(s: &str) -> Url {
        Url::parse(s).expect("valid test URL")
    }

    fn decide_with(u: &Url, config: &Config, context: Context<'_>) -> Result<(), Skip> {
        let backend = backend();
        decide(u, config, &[&backend], context)
    }

    #[test]
    fn an_ordinary_url_is_shortened() {
        let config = Config::default();
        assert_eq!(
            decide_with(&url("https://example.com/a"), &config, Context::default()),
            Ok(())
        );
    }

    #[test]
    fn nothing_happens_while_disabled() {
        let config = Config {
            auto_shorten: false,
            ..Config::default()
        };
        assert_eq!(
            decide_with(&url("https://example.com/a"), &config, Context::default()),
            Err(Skip::Disabled)
        );
    }

    #[test]
    fn nothing_happens_without_a_backend() {
        let config = Config::default();
        assert_eq!(
            decide(
                &url("https://example.com/a"),
                &config,
                &[],
                Context::default()
            ),
            Err(Skip::NoBackends)
        );
    }

    #[test]
    fn the_app_never_shortens_its_own_output() {
        // The write-back from a previous shorten triggers a clipboard change;
        // without this the app would shorten in a loop.
        let config = Config::default();
        let short = url("https://s.example/abc");
        let context = Context {
            last_written: Some(short.as_str()),
            ..Context::default()
        };
        assert_eq!(decide_with(&short, &config, context), Err(Skip::OwnOutput));
    }

    #[test]
    fn a_link_from_a_configured_shortener_is_left_alone() {
        let config = Config::default();
        assert_eq!(
            decide_with(&url("https://s.example/xyz"), &config, Context::default()),
            Err(Skip::AlreadyShort)
        );
    }

    #[test]
    fn copying_the_same_url_twice_leaves_it_alone() {
        let config = Config::default();
        let target = url("https://example.com/a");
        let context = Context {
            last_seen: Some(target.as_str()),
            ..Context::default()
        };
        assert_eq!(
            decide_with(&target, &config, context),
            Err(Skip::CopiedTwice)
        );
    }

    #[test]
    fn the_double_copy_rule_can_be_switched_off() {
        let config = Config {
            bypass_double_copy: false,
            ..Config::default()
        };
        let target = url("https://example.com/a");
        let context = Context {
            last_seen: Some(target.as_str()),
            ..Context::default()
        };
        assert_eq!(decide_with(&target, &config, context), Ok(()));
    }

    #[test]
    fn holding_the_bypass_key_skips_one_url() {
        let config = Config::default();
        let context = Context {
            bypass_held: true,
            ..Context::default()
        };
        assert_eq!(
            decide_with(&url("https://example.com/a"), &config, context),
            Err(Skip::BypassHeld)
        );
    }

    #[test]
    fn the_bypass_key_can_be_switched_off() {
        let config = Config {
            bypass_shift: false,
            ..Config::default()
        };
        let context = Context {
            bypass_held: true,
            ..Context::default()
        };
        assert_eq!(
            decide_with(&url("https://example.com/a"), &config, context),
            Ok(())
        );
    }

    #[test]
    fn scroll_lock_disables_shortening() {
        let config = Config::default();
        let context = Context {
            scroll_lock: true,
            ..Context::default()
        };
        assert_eq!(
            decide_with(&url("https://example.com/a"), &config, context),
            Err(Skip::ScrollLock)
        );
    }

    #[test]
    fn a_blacklisted_url_is_skipped() {
        let config = Config {
            blacklist_regex: r"^https://internal\.".to_string(),
            ..Config::default()
        };
        assert_eq!(
            decide_with(
                &url("https://internal.example/a"),
                &config,
                Context::default()
            ),
            Err(Skip::Blacklisted)
        );
        assert_eq!(
            decide_with(&url("https://example.com/a"), &config, Context::default()),
            Ok(())
        );
    }

    #[test]
    fn an_invalid_blacklist_does_not_block_everything() {
        // Validated at load, but a reload could race; failing open is better
        // than silently shortening nothing.
        let config = Config {
            blacklist_regex: "([".to_string(),
            ..Config::default()
        };
        assert_eq!(
            decide_with(&url("https://example.com/a"), &config, Context::default()),
            Ok(())
        );
    }

    #[test]
    fn every_skip_reason_has_a_message() {
        for skip in [
            Skip::Disabled,
            Skip::NoBackends,
            Skip::BypassHeld,
            Skip::ScrollLock,
            Skip::Blacklisted,
            Skip::AlreadyShort,
            Skip::OwnOutput,
            Skip::CopiedTwice,
        ] {
            assert!(!skip.reason().is_empty());
        }
    }
}

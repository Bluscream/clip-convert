//! URL shortening, with pluggable backends.
//!
//! A backend is either a YOURLS instance or any program that reads a URL on
//! stdin and prints the short one, so a service this app has never heard of can
//! be added with a three-line shell script and a config entry.

use crate::config::{Shortener, ShortenerKind};
use rand::seq::SliceRandom;
use std::time::Duration;
use url::Url;

/// How long a shortener may take before the attempt is abandoned.
const TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, thiserror::Error)]
pub enum ShortenError {
    #[error("no shortener is configured; add a `[[shorteners]]` entry")]
    NoneConfigured,
    #[error("shortener `{name}` failed: {source}")]
    Backend {
        name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("shortener `{name}` did not return a URL: {response}")]
    NotAUrl { name: String, response: String },
}

/// Picks a shortener at random from the enabled ones.
///
/// Random rather than round-robin so that the choice carries no state to persist
/// and successive links do not reveal a predictable rotation.
#[must_use]
pub fn pick<'a>(shorteners: &[&'a Shortener]) -> Option<&'a Shortener> {
    shorteners.choose(&mut rand::thread_rng()).copied()
}

/// Shortens `url` using `shortener`.
///
/// # Errors
///
/// Returns [`ShortenError::Backend`] if the request or command failed, and
/// [`ShortenError::NotAUrl`] if the response was not a usable http(s) URL —
/// which is how YOURLS reports an authentication failure, as a plain-text error
/// body with a success status.
pub fn shorten(url: &Url, shortener: &Shortener, ignore_ssl: bool) -> Result<Url, ShortenError> {
    let response = match shortener.kind {
        ShortenerKind::Yourls => via_yourls(url, shortener, ignore_ssl)?,
        ShortenerKind::Command => via_command(url, shortener)?,
    };

    parse_response(&response).ok_or_else(|| ShortenError::NotAUrl {
        name: shortener.name.clone(),
        // Truncated: a failing endpoint may return an entire HTML error page,
        // and this string ends up in a notification.
        response: crate::text::truncate(&response, 200, "..."),
    })
}

fn via_yourls(url: &Url, shortener: &Shortener, ignore_ssl: bool) -> Result<String, ShortenError> {
    let fail = |e: ureq::Error| ShortenError::Backend {
        name: shortener.name.clone(),
        source: Box::new(e),
    };

    // Built with the URL type rather than string concatenation so the target URL
    // and the signature are escaped correctly.
    let mut endpoint = Url::parse(&shortener.api_url).map_err(|e| ShortenError::Backend {
        name: shortener.name.clone(),
        source: Box::new(e),
    })?;
    endpoint
        .query_pairs_mut()
        .append_pair("signature", &shortener.signature)
        .append_pair("action", "shorturl")
        .append_pair("format", "simple")
        .append_pair("url", url.as_str());

    let response = agent(ignore_ssl)
        .request_url("GET", &endpoint)
        .timeout(TIMEOUT)
        .call()
        .map_err(fail)?;

    response
        .into_string()
        .map(|s| s.trim().to_string())
        .map_err(|e| ShortenError::Backend {
            name: shortener.name.clone(),
            source: Box::new(e),
        })
}

fn via_command(url: &Url, shortener: &Shortener) -> Result<String, ShortenError> {
    let out = crate::exec::run(&shortener.command, Some(url.as_str().as_bytes()), TIMEOUT)
        .map_err(|source| ShortenError::Backend {
            name: shortener.name.clone(),
            source: Box::new(source),
        })?;
    Ok(out.stdout_text())
}

/// Accepts a backend's response only if it is a usable http(s) URL.
fn parse_response(response: &str) -> Option<Url> {
    let trimmed = response.trim();
    if trimmed.chars().any(char::is_whitespace) {
        return None;
    }
    let url = Url::parse(trimmed).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

/// Whether `url` is already a link served by one of the configured shorteners.
///
/// Prevents the auto-shorten path from shortening its own output, which would
/// otherwise happen the moment the short URL is written back to the clipboard.
#[must_use]
pub fn is_already_short(url: &Url, shorteners: &[&Shortener]) -> bool {
    shorteners.iter().any(|s| {
        let base = s.base_url.trim();
        !base.is_empty() && url.as_str().starts_with(base)
    })
}

fn agent(ignore_ssl: bool) -> ureq::Agent {
    let mut builder = ureq::builder();

    if ignore_ssl {
        let mut tls = native_tls::TlsConnector::builder();
        tls.danger_accept_invalid_certs(true);
        tls.danger_accept_invalid_hostnames(true);
        match tls.build() {
            Ok(connector) => builder = builder.tls_connector(std::sync::Arc::new(connector)),
            Err(e) => log::warn!("could not build a permissive TLS connector: {e}"),
        }
    }

    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortener(name: &str, base: &str) -> Shortener {
        Shortener {
            name: name.to_string(),
            kind: ShortenerKind::Yourls,
            api_url: format!("{base}yourls-api.php"),
            signature: "secret".to_string(),
            base_url: base.to_string(),
            command: Vec::new(),
            enabled: true,
        }
    }

    #[test]
    fn a_valid_response_is_accepted() {
        assert_eq!(
            parse_response("https://s.example/abc").map(|u| u.to_string()),
            Some("https://s.example/abc".to_string())
        );
        assert!(parse_response("  http://s.example/a \n").is_some());
    }

    #[test]
    fn a_yourls_error_body_is_not_mistaken_for_a_url() {
        // YOURLS answers an auth failure with a plain-text body and HTTP 200.
        for body in [
            "",
            "error: please log in",
            "Invalid signature",
            "<html><body>500</body></html>",
        ] {
            assert!(
                parse_response(body).is_none(),
                "{body:?} should not parse as a short URL"
            );
        }
    }

    #[test]
    fn a_non_web_scheme_response_is_rejected() {
        assert!(parse_response("file:///etc/passwd").is_none());
        assert!(parse_response("javascript:alert(1)").is_none());
    }

    #[test]
    fn already_shortened_links_are_recognised() {
        let a = shortener("a", "https://s.example/");
        let b = shortener("b", "https://t.example/");
        let list = vec![&a, &b];

        let short = Url::parse("https://t.example/xyz").expect("valid");
        assert!(is_already_short(&short, &list));

        let long = Url::parse("https://example.com/a/very/long/path").expect("valid");
        assert!(!is_already_short(&long, &list));
    }

    #[test]
    fn a_shortener_without_a_base_url_never_matches() {
        // An empty base_url must not make every URL look already-shortened.
        let mut s = shortener("a", "");
        s.base_url = String::new();
        let list = vec![&s];
        let url = Url::parse("https://example.com/x").expect("valid");
        assert!(!is_already_short(&url, &list));
    }

    #[test]
    fn picking_from_an_empty_list_yields_nothing() {
        assert!(pick(&[]).is_none());
    }

    #[test]
    fn picking_from_one_always_yields_it() {
        let only = shortener("only", "https://s.example/");
        let list = vec![&only];
        for _ in 0..20 {
            assert_eq!(pick(&list).map(|s| s.name.as_str()), Some("only"));
        }
    }

    #[test]
    fn picking_eventually_reaches_every_shortener() {
        let a = shortener("a", "https://a.example/");
        let b = shortener("b", "https://b.example/");
        let list = vec![&a, &b];

        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..200 {
            if let Some(chosen) = pick(&list) {
                seen.insert(chosen.name.clone());
            }
        }
        assert_eq!(seen.len(), 2, "random pick never selected one of the two");
    }

    #[test]
    fn a_command_backend_shortens_through_its_program() {
        let backend = Shortener {
            name: "fake".to_string(),
            kind: ShortenerKind::Command,
            api_url: String::new(),
            signature: String::new(),
            base_url: "https://s.example/".to_string(),
            command: vec!["echo".to_string(), "https://s.example/ok".to_string()],
            enabled: true,
        };
        let url = Url::parse("https://example.com/long").expect("valid");
        let short = shorten(&url, &backend, false).expect("command backend works");
        assert_eq!(short.as_str(), "https://s.example/ok");
    }

    #[test]
    fn a_command_backend_that_prints_junk_is_reported_not_used() {
        let backend = Shortener {
            name: "fake".to_string(),
            kind: ShortenerKind::Command,
            api_url: String::new(),
            signature: String::new(),
            base_url: String::new(),
            command: vec!["echo".to_string(), "not a url".to_string()],
            enabled: true,
        };
        let url = Url::parse("https://example.com/long").expect("valid");
        match shorten(&url, &backend, false) {
            Err(ShortenError::NotAUrl { name, .. }) => assert_eq!(name, "fake"),
            other => panic!("expected NotAUrl, got {other:?}"),
        }
    }

    #[test]
    fn a_command_backend_that_fails_is_reported_by_name() {
        let backend = Shortener {
            name: "broken".to_string(),
            kind: ShortenerKind::Command,
            api_url: String::new(),
            signature: String::new(),
            base_url: String::new(),
            command: vec!["lcc-no-such-program".to_string()],
            enabled: true,
        };
        let url = Url::parse("https://example.com/long").expect("valid");
        match shorten(&url, &backend, false) {
            Err(ShortenError::Backend { name, .. }) => assert_eq!(name, "broken"),
            other => panic!("expected Backend, got {other:?}"),
        }
    }

    #[test]
    fn the_target_url_is_escaped_into_the_query_rather_than_concatenated() {
        let mut endpoint = Url::parse("https://s.example/yourls-api.php").expect("valid");
        let target = "https://example.com/a?b=c&d=e#f";
        endpoint
            .query_pairs_mut()
            .append_pair("action", "shorturl")
            .append_pair("url", target);

        // The nested query must not leak into the outer one as extra parameters.
        let pairs: Vec<(String, String)> = endpoint
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[1], ("url".to_string(), target.to_string()));
    }
}

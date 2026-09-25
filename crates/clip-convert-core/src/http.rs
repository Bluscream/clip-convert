//! The one place an HTTP client is built.
//!
//! `ureq` is compiled here with `native-tls` and without its own bundled
//! backend, which means **it cannot make an HTTPS request until a connector is
//! installed on the agent**. A default `ureq::get` fails on every `https://`
//! URL with "no TLS backend is configured", which is a runtime failure with no
//! compile-time warning — so every request in this crate comes from here.

use std::sync::Arc;
use std::time::Duration;

/// Builds an agent that can actually speak HTTPS.
///
/// `ignore_ssl` makes it accept invalid certificates, which exists for a host
/// with a self-signed certificate and nothing else.
#[must_use]
pub fn agent(ignore_ssl: bool, timeout: Duration) -> ureq::Agent {
    let mut tls = native_tls::TlsConnector::builder();
    if ignore_ssl {
        tls.danger_accept_invalid_certs(true);
        tls.danger_accept_invalid_hostnames(true);
    }

    let mut builder = ureq::builder().timeout(timeout);
    match tls.build() {
        Ok(connector) => builder = builder.tls_connector(Arc::new(connector)),
        // Without a connector every HTTPS request fails, so this is worth
        // saying loudly rather than discovering one request later.
        Err(e) => log::error!("could not build a TLS connector; HTTPS will not work: {e}"),
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_is_built_for_both_strictness_levels() {
        // Construction alone is the check: a missing connector is what breaks
        // HTTPS, and it cannot be observed without making a request.
        let _strict = agent(false, Duration::from_secs(5));
        let _permissive = agent(true, Duration::from_secs(5));
    }
}

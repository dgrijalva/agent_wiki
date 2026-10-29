//! The `Host` check against DNS rebinding: a page on another site whose name
//! resolves to this server still addresses its requests to that name, so
//! only requests addressed to a host the wiki is known by are served.

use std::fmt;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::uri::Authority;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The hosts a wiki answers to, checked against every request's `Host`; set
/// with [`WikiBuilder::allowed_hosts`](crate::WikiBuilder::allowed_hosts).
#[derive(Clone)]
pub struct AllowedHosts {
    check: Check,
}

#[derive(Clone)]
enum Check {
    Any,
    List(Vec<Allowed>),
    Fn(Arc<dyn Fn(&str) -> bool + Send + Sync>),
}

#[derive(Debug, Clone)]
struct Allowed {
    host: String,
    /// `None` matches any port.
    port: Option<u16>,
}

impl AllowedHosts {
    /// `localhost`, `127.0.0.1` and `[::1]`, on any port.
    pub fn loopback() -> Self {
        Self::new(["localhost", "127.0.0.1", "[::1]"])
    }

    /// These hosts: a name or address (`wiki.example.com`, `[::1]`) allows
    /// any port, and `host:port` only that port. Case is ignored.
    pub fn new(hosts: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let hosts = hosts
            .into_iter()
            .map(|entry| {
                let entry = entry.as_ref().trim();
                match Authority::try_from(entry) {
                    Ok(authority) => Allowed {
                        host: normalize(authority.host()),
                        port: authority.port_u16(),
                    },
                    // An unbracketed IPv6 address, e.g. `::1`.
                    Err(_) => Allowed {
                        host: normalize(entry),
                        port: None,
                    },
                }
            })
            .collect();
        AllowedHosts {
            check: Check::List(hosts),
        }
    }

    /// Every host: no check, on the MCP endpoint either.
    pub fn any() -> Self {
        AllowedHosts { check: Check::Any }
    }

    /// The hosts `allow` returns `true` for. It receives the host without
    /// its port, lowercased, an IPv6 address without brackets (`::1`); a
    /// request naming no host is refused without calling it.
    pub fn from_fn(allow: impl Fn(&str) -> bool + Send + Sync + 'static) -> Self {
        AllowedHosts {
            check: Check::Fn(Arc::new(allow)),
        }
    }

    /// Whether a request addressed to `authority` (a `Host` value, or
    /// `None` when the request names no host) is served.
    fn allows(&self, authority: Option<&str>) -> bool {
        if let Check::Any = self.check {
            return true;
        }
        let Some(authority) = authority.and_then(|a| Authority::try_from(a).ok()) else {
            return false;
        };
        let host = normalize(authority.host());
        let port = authority.port_u16();
        match &self.check {
            Check::Any => true,
            Check::List(hosts) => hosts.iter().any(|allowed| {
                allowed.host == host && allowed.port.is_none_or(|p| port == Some(p))
            }),
            Check::Fn(allow) => allow(&host),
        }
    }
}

impl fmt::Debug for AllowedHosts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.check {
            Check::Any => f.write_str("AllowedHosts::any()"),
            Check::List(hosts) => f.debug_list().entries(hosts).finish(),
            Check::Fn(_) => f.write_str("AllowedHosts::from_fn(..)"),
        }
    }
}

fn normalize(host: &str) -> String {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
}

/// Middleware refusing, with `403`, a request whose host isn't allowed.
pub(crate) async fn check(
    State(hosts): State<AllowedHosts>,
    request: Request,
    next: Next,
) -> Response {
    // HTTP/2 carries the host in `:authority`, which may come without a
    // `Host` header.
    let authority = match request.headers().get(header::HOST) {
        Some(value) => value.to_str().ok(),
        None => request.uri().authority().map(Authority::as_str),
    };
    if hosts.allows(authority) {
        next.run(request).await
    } else {
        (
            StatusCode::FORBIDDEN,
            "Forbidden: Host header is not allowed",
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_on_any_port() {
        let hosts = AllowedHosts::loopback();
        for host in [
            "localhost",
            "localhost:3000",
            "LocalHost:80",
            "127.0.0.1:3000",
            "[::1]",
            "[::1]:3000",
        ] {
            assert!(hosts.allows(Some(host)), "{host}");
        }
        for host in [
            "evil.example",
            "evil.example:3000",
            "localhost.evil.example",
            "127.0.0.2",
            "",
            "local host",
        ] {
            assert!(!hosts.allows(Some(host)), "{host}");
        }
        assert!(!hosts.allows(None));
    }

    #[test]
    fn a_port_in_an_entry_allows_only_that_port() {
        let hosts = AllowedHosts::new(["wiki.example.com:8080", "::1"]);
        assert!(hosts.allows(Some("wiki.example.com:8080")));
        assert!(!hosts.allows(Some("wiki.example.com")));
        assert!(!hosts.allows(Some("wiki.example.com:3000")));
        assert!(hosts.allows(Some("[::1]:3000")));
    }

    #[test]
    fn any_allows_every_host_and_none() {
        let hosts = AllowedHosts::any();
        assert!(hosts.allows(Some("evil.example")));
        assert!(hosts.allows(None));
    }

    #[test]
    fn from_fn_receives_the_normalized_host() {
        let hosts =
            AllowedHosts::from_fn(|host| host.ends_with(".tailnet.example") || host == "::1");
        assert!(hosts.allows(Some("wiki.tailnet.example")));
        assert!(hosts.allows(Some("Wiki.Tailnet.Example:3000")));
        assert!(hosts.allows(Some("[::1]:3000")));
        assert!(!hosts.allows(Some("evil.example")));
        assert!(!hosts.allows(Some("tailnet.example.evil:3000")));
    }

    #[test]
    fn from_fn_is_not_called_without_a_host() {
        let hosts = AllowedHosts::from_fn(|_| panic!("called without a host"));
        assert!(!hosts.allows(None));
        assert!(!hosts.allows(Some("local host")));
    }
}

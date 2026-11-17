//! The proxy server set in the settings.
//!
//! Playlists, guides and logos (ureq) can use HTTP and SOCKS proxies;
//! streams (mpv/FFmpeg) only HTTP ones. The address may hold a user name
//! and password, so [`ProxyUrl::describe`] is what goes into logs.

use std::fmt;
use std::sync::RwLock;

/// The kind of proxy server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// HTTP proxy (`CONNECT` for HTTPS).
    Http,
    Socks4,
    /// SOCKS4a: the proxy looks up host names.
    Socks4a,
    Socks5,
    /// SOCKS5 with host names looked up by the proxy.
    Socks5h,
}

impl Protocol {
    fn scheme(self) -> &'static str {
        match self {
            Protocol::Http => "http",
            Protocol::Socks4 => "socks4",
            Protocol::Socks4a => "socks4a",
            Protocol::Socks5 => "socks5",
            Protocol::Socks5h => "socks5h",
        }
    }

    fn from_scheme(scheme: &str) -> Option<Self> {
        Some(match scheme {
            "http" => Protocol::Http,
            "socks4" => Protocol::Socks4,
            "socks4a" => Protocol::Socks4a,
            "socks5" | "socks" => Protocol::Socks5,
            "socks5h" => Protocol::Socks5h,
            _ => return None,
        })
    }

    fn default_port(self) -> u16 {
        match self {
            Protocol::Http => 8080,
            _ => 1080,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyError {
    UnknownScheme(String),
    MissingHost,
    BadPort,
    /// Paths, queries and the like have no meaning for a proxy.
    ExtraParts,
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProxyError::UnknownScheme(s) => write!(
                f,
                "unknown proxy type \"{s}\" (use http, socks4, socks4a, socks5 or socks5h)"
            ),
            ProxyError::MissingHost => write!(f, "the proxy address has no host"),
            ProxyError::BadPort => write!(f, "the proxy port must be a number from 1 to 65535"),
            ProxyError::ExtraParts => write!(f, "a proxy address is only host and port"),
        }
    }
}

impl std::error::Error for ProxyError {}

/// A checked proxy address: `scheme://[user[:password]@]host:port`.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyUrl {
    protocol: Protocol,
    /// `user[:password]`, as typed (percent-encoded if needed).
    userinfo: Option<String>,
    host: String,
    port: u16,
}

// Hand-written so that `{:?}` never prints the password.
impl fmt::Debug for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProxyUrl({})", self.describe())
    }
}

impl ProxyUrl {
    /// Reads what was typed into the settings. Empty text means no proxy
    /// (`Ok(None)`). Without a scheme, `http` is assumed; without a port,
    /// 8080 (HTTP) or 1080 (SOCKS).
    pub fn parse(input: &str) -> Result<Option<Self>, ProxyError> {
        let input = input.trim();
        if input.is_empty() {
            return Ok(None);
        }
        let (protocol, rest) = match input.split_once("://") {
            Some((scheme, rest)) => {
                let scheme = scheme.to_ascii_lowercase();
                let protocol =
                    Protocol::from_scheme(&scheme).ok_or(ProxyError::UnknownScheme(scheme))?;
                (protocol, rest)
            }
            None => (Protocol::Http, input),
        };
        // A single trailing slash is harmless ("http://proxy:3128/").
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        if rest.contains(['/', '?', '#', ' ']) {
            return Err(ProxyError::ExtraParts);
        }
        // The password may contain '@'; the host never does.
        let (userinfo, hostport) = match rest.rsplit_once('@') {
            Some((user, hostport)) => ((!user.is_empty()).then(|| user.to_string()), hostport),
            None => (None, rest),
        };
        let (host, port) = split_host_port(hostport)?;
        if host.is_empty() {
            return Err(ProxyError::MissingHost);
        }
        Ok(Some(ProxyUrl {
            protocol,
            userinfo,
            host: host.to_string(),
            port: port.unwrap_or(protocol.default_port()),
        }))
    }

    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// The full address with scheme and port (contains the password).
    pub fn url(&self) -> String {
        let user = self
            .userinfo
            .as_deref()
            .map(|u| format!("{u}@"))
            .unwrap_or_default();
        format!(
            "{}://{user}{}:{}",
            self.protocol.scheme(),
            self.host,
            self.port
        )
    }

    /// For logs: the address without user name and password.
    pub fn describe(&self) -> String {
        let login = if self.userinfo.is_some() {
            " (with login)"
        } else {
            ""
        };
        format!(
            "{}://{}:{}{login}",
            self.protocol.scheme(),
            self.host,
            self.port
        )
    }

    /// The value of mpv's `http-proxy` option: only HTTP proxies work for
    /// streams (FFmpeg has no SOCKS support).
    pub fn mpv_value(&self) -> Option<String> {
        (self.protocol == Protocol::Http).then(|| self.url())
    }

    fn to_ureq(&self) -> Option<ureq::Proxy> {
        ureq::Proxy::new(&self.url()).ok()
    }
}

/// `host`, `host:port`, `[v6]` or `[v6]:port`.
fn split_host_port(text: &str) -> Result<(&str, Option<u16>), ProxyError> {
    let (host, port) = if let Some(rest) = text.strip_prefix('[') {
        // IPv6 literal: keep the brackets, a URL needs them.
        let end = rest.find(']').ok_or(ProxyError::MissingHost)?;
        let host = &text[..end + 2];
        match &rest[end + 1..] {
            "" => (host, None),
            p => (host, Some(p.strip_prefix(':').ok_or(ProxyError::BadPort)?)),
        }
    } else {
        match text.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (text, None),
        }
    };
    let port = match port {
        None => None,
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 => Some(n),
            _ => return Err(ProxyError::BadPort),
        },
    };
    Ok((host, port))
}

/// The proxy for downloads. `None` leaves ureq's default, which follows
/// the `ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY` environment variables.
static CURRENT: RwLock<Option<ProxyUrl>> = RwLock::new(None);

/// Sets the proxy for all later downloads (playlists, guides, logos).
pub fn set_current(proxy: Option<ProxyUrl>) {
    // A poisoned lock still holds a valid value.
    *CURRENT.write().unwrap_or_else(|p| p.into_inner()) = proxy;
}

/// The ureq proxy to use for the next download, if one is set.
pub(crate) fn current_for_ureq() -> Option<ureq::Proxy> {
    CURRENT
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(ProxyUrl::to_ureq)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> ProxyUrl {
        ProxyUrl::parse(s).unwrap().unwrap()
    }

    #[test]
    fn empty_means_no_proxy() {
        assert_eq!(ProxyUrl::parse(""), Ok(None));
        assert_eq!(ProxyUrl::parse("   "), Ok(None));
    }

    #[test]
    fn scheme_and_port_have_defaults() {
        assert_eq!(
            parse("proxy.example.com").url(),
            "http://proxy.example.com:8080"
        );
        assert_eq!(
            parse("proxy.example.com:3128").url(),
            "http://proxy.example.com:3128"
        );
        assert_eq!(parse("socks5://127.0.0.1").url(), "socks5://127.0.0.1:1080");
        assert_eq!(
            parse("SOCKS://127.0.0.1:9050/").url(),
            "socks5://127.0.0.1:9050"
        );
        assert_eq!(parse("socks5h://[::1]:9050").url(), "socks5h://[::1]:9050");
        assert_eq!(parse("http://[::1]").url(), "http://[::1]:8080");
    }

    #[test]
    fn login_is_kept_but_never_described() {
        let p = parse("http://user:fakepass@proxy.example.com:3128");
        assert_eq!(p.url(), "http://user:fakepass@proxy.example.com:3128");
        let described = p.describe();
        assert_eq!(described, "http://proxy.example.com:3128 (with login)");
        assert!(!format!("{p:?}").contains("fakepass"));
        // '@' inside the password.
        let p = parse("socks5://me:p@ss@proxy.example.com");
        assert_eq!(p.url(), "socks5://me:p@ss@proxy.example.com:1080");
    }

    #[test]
    fn bad_addresses_are_rejected() {
        assert_eq!(
            ProxyUrl::parse("ftp://proxy.example.com"),
            Err(ProxyError::UnknownScheme("ftp".into()))
        );
        assert_eq!(
            ProxyUrl::parse("https://proxy.example.com"),
            Err(ProxyError::UnknownScheme("https".into()))
        );
        assert_eq!(ProxyUrl::parse("http://"), Err(ProxyError::MissingHost));
        assert_eq!(
            ProxyUrl::parse("http://:8080"),
            Err(ProxyError::MissingHost)
        );
        assert_eq!(
            ProxyUrl::parse("proxy.example.com:0"),
            Err(ProxyError::BadPort)
        );
        assert_eq!(
            ProxyUrl::parse("proxy.example.com:99999"),
            Err(ProxyError::BadPort)
        );
        assert_eq!(
            ProxyUrl::parse("proxy.example.com:abc"),
            Err(ProxyError::BadPort)
        );
        assert_eq!(
            ProxyUrl::parse("http://proxy.example.com/path"),
            Err(ProxyError::ExtraParts)
        );
    }

    #[test]
    fn only_http_proxies_reach_mpv() {
        assert_eq!(
            parse("proxy.example.com:3128").mpv_value().as_deref(),
            Some("http://proxy.example.com:3128")
        );
        assert_eq!(parse("socks5://127.0.0.1").mpv_value(), None);
    }

    #[test]
    fn every_accepted_address_is_valid_for_ureq() {
        for s in [
            "proxy.example.com",
            "http://u:p@proxy.example.com:3128",
            "socks4://10.0.0.1",
            "socks4a://proxy.example.com",
            "socks5://u:p@proxy.example.com:1080",
            "socks5h://[::1]:9050",
        ] {
            assert!(parse(s).to_ureq().is_some(), "{s}");
        }
    }
}

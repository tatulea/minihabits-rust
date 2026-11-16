//! HTTP trust boundary. A configured canonical origin is authoritative; neither
//! Host nor forwarding headers can redefine it. Deployment policy is separate
//! from account/session storage so proxy details cannot leak into domain rules.
use axum::{
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::net::SocketAddr;

#[derive(Clone, Debug)]
pub struct SecurityConfig {
    pub origin: String,
    authority: String,
    pub secure_cookies: bool,
}

impl SecurityConfig {
    pub fn new(origin: &str) -> Result<Self, String> {
        if origin.len() > 256 {
            return Err("MINIHABITS_ORIGIN is too long.".into());
        }
        let url = url::Url::parse(origin)
            .map_err(|_| "MINIHABITS_ORIGIN must be an absolute HTTP(S) origin.")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "MINIHABITS_ORIGIN must contain only scheme, hostname, and optional port.".into(),
            );
        }
        let secure_cookies = url.scheme() == "https";
        let local_origin = match url.host() {
            Some(url::Host::Domain("localhost")) => true,
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if !secure_cookies && !local_origin {
            return Err(
                "HTTP origins are only supported for localhost or loopback IP addresses. Use an HTTPS origin for deployment.".into(),
            );
        }
        let origin = url.origin().ascii_serialization();
        let authority = origin
            .split_once("://")
            .expect("validated scheme")
            .1
            .to_owned();
        Ok(Self {
            origin,
            authority,
            secure_cookies,
        })
    }

    pub fn from_env() -> Result<Self, String> {
        let origin = std::env::var("MINIHABITS_ORIGIN")
            .map_err(|_| "Set MINIHABITS_ORIGIN to the exact browser origin, for example https://habits.home.example.")?;
        Self::new(&origin)
    }

    pub fn validate_bind_addr(&self, addr: SocketAddr) -> Result<(), String> {
        if !self.secure_cookies && !addr.ip().is_loopback() {
            return Err(
                "HTTP development requires a loopback LEPTOS_SITE_ADDR. Use an HTTPS origin behind a proxy for deployment.".into(),
            );
        }
        Ok(())
    }

    pub fn cookie_name(&self) -> &'static str {
        if self.secure_cookies {
            "__Host-minihabits_session"
        } else {
            "minihabits_session"
        }
    }

    /// Reject ambiguous same-name cookies before the library parser chooses
    /// one. Malformed IDs themselves are handled by tower-sessions as anonymous.
    fn unambiguous_cookie(&self, headers: &HeaderMap) -> bool {
        let mut count = 0;
        for value in headers.get_all(header::COOKIE) {
            let Ok(value) = value.to_str() else {
                return false;
            };
            for cookie in value.split(';') {
                if cookie
                    .trim()
                    .split_once('=')
                    .is_some_and(|(name, _)| name == self.cookie_name())
                {
                    count += 1;
                }
            }
        }
        count <= 1
    }

    pub fn allows(&self, headers: &HeaderMap, method: &Method) -> bool {
        if !self.unambiguous_cookie(headers)
            || headers.get_all(header::HOST).iter().count() != 1
            || headers.get(header::HOST).and_then(|h| h.to_str().ok())
                != Some(self.authority.as_str())
        {
            return false;
        }
        if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
            if headers.get_all(header::ORIGIN).iter().count() != 1
                || headers.get(header::ORIGIN).and_then(|h| h.to_str().ok())
                    != Some(self.origin.as_str())
            {
                return false;
            }
            if headers
                .get("sec-fetch-site")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|v| v != "same-origin" && v != "none")
            {
                return false;
            }
        }
        true
    }
}

/// Socket peer only. Never trust arbitrary X-Forwarded-For for rate-limit keys.
/// A reverse proxy therefore shares a conservative IP bucket unless separately
/// enforcing its own per-client limits. The account/global buckets still apply.
pub fn peer_key(parts: &axum::http::request::Parts) -> String {
    parts
        .extensions
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|peer| peer.0.ip().to_string())
        .unwrap_or_else(|| "unknown-peer".into())
}

pub async fn request_guard(
    State(config): State<SecurityConfig>,
    request: Request,
    next: Next,
) -> Response {
    if !config.allows(request.headers(), request.method()) {
        return (
            StatusCode::FORBIDDEN,
            "Request origin does not match this app's configured address.",
        )
            .into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    // tower-sessions 0.14 removes a clone of the incoming cookie without its
    // original Secure flag. Browsers reject that deletion for __Host- names.
    // Normalize only our library-emitted cookies, including expiry/logout, so
    // the same policy applies to creation and deletion. The library still owns
    // IDs, expiry, serialization and all lifecycle operations.
    let cookies: Vec<_> = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .cloned()
        .collect();
    headers.remove(header::SET_COOKIE);
    for original in cookies {
        let hardened = original
            .to_str()
            .ok()
            .and_then(|value| tower_sessions::cookie::Cookie::parse(value.to_owned()).ok())
            .filter(|cookie| cookie.name() == config.cookie_name())
            .and_then(|mut cookie| {
                cookie.set_secure(config.secure_cookies);
                cookie.set_http_only(true);
                cookie.set_same_site(tower_sessions::cookie::SameSite::Lax);
                cookie.set_path("/");
                cookie.unset_domain();
                cookie.to_string().parse().ok()
            });
        headers.append(header::SET_COOKIE, hardened.unwrap_or(original));
    }
    for (name, value) in [
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "same-origin"),
        (header::CACHE_CONTROL, "no-store"),
    ] {
        headers.insert(name, value.parse().expect("fixed header"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    fn headers(host: &str, origin: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, host.parse().unwrap());
        if let Some(origin) = origin {
            h.insert(header::ORIGIN, origin.parse().unwrap());
        }
        h
    }
    #[test]
    fn canonical_origin_is_explicit_and_fail_closed() {
        for invalid in [
            "https://a/b",
            "https://user@a",
            "https://a/?x=1",
            "https://a/#x",
            "ftp://a",
        ] {
            assert!(SecurityConfig::new(invalid).is_err());
        }
        let c = SecurityConfig::new("https://habits.home.example").unwrap();
        assert!(c.allows(&headers("habits.home.example", None), &Method::GET));
        assert!(c.allows(
            &headers("habits.home.example", Some(&c.origin)),
            &Method::POST
        ));
        for origin in [
            None,
            Some("null"),
            Some("https://evil.example"),
            Some("https://habits.home.example.evil.example"),
            Some("http://habits.home.example"),
        ] {
            assert!(!c.allows(&headers("habits.home.example", origin), &Method::POST));
        }
        assert!(!c.allows(&headers("evil.example", Some(&c.origin)), &Method::POST));
        let mut h = headers("habits.home.example", Some(&c.origin));
        h.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(!c.allows(&h, &Method::POST));
    }
    #[test]
    fn cookie_security_follows_the_origin_scheme() {
        for origin in [
            "http://localhost:5175",
            "http://127.0.0.1:5175",
            "http://[::1]:5175",
        ] {
            let config = SecurityConfig::new(origin).unwrap();
            assert!(!config.secure_cookies);
            assert_eq!(config.cookie_name(), "minihabits_session");
            assert!(config.allows(&headers(&config.authority, Some(origin)), &Method::POST));
        }
        for origin in [
            "https://localhost:5175",
            "https://habits.home.example",
            "HTTPS://habits.home.example",
        ] {
            let config = SecurityConfig::new(origin).unwrap();
            assert!(config.secure_cookies);
            assert_eq!(config.cookie_name(), "__Host-minihabits_session");
        }
    }
    #[test]
    fn http_requires_a_local_origin_and_loopback_listener() {
        for origin in [
            "http://192.168.1.5:5175",
            "http://habits.home.example",
            "http://localhost.evil.example",
            "http://0.0.0.0:5175",
            "http://[::]:5175",
        ] {
            assert!(SecurityConfig::new(origin).is_err());
        }
        let local = SecurityConfig::new("http://localhost:5175").unwrap();
        let proxied = SecurityConfig::new("https://habits.home.example").unwrap();
        for addr in ["127.0.0.1:5175", "[::1]:5175"] {
            assert!(local.validate_bind_addr(addr.parse().unwrap()).is_ok());
            assert!(proxied.validate_bind_addr(addr.parse().unwrap()).is_ok());
        }
        for addr in ["0.0.0.0:5175", "[::]:5175", "192.168.1.5:5175"] {
            assert!(local.validate_bind_addr(addr.parse().unwrap()).is_err());
            assert!(proxied.validate_bind_addr(addr.parse().unwrap()).is_ok());
        }
    }
    #[test]
    fn ambiguous_cookies_are_rejected_before_library_parsing() {
        let secure = SecurityConfig::new("https://habits.home.example").unwrap();
        assert!(secure.cookie_name().starts_with("__Host-"));
        let mut h = headers("habits.home.example", Some(&secure.origin));
        h.insert(
            header::COOKIE,
            "__Host-minihabits_session=first; __Host-minihabits_session=second"
                .parse()
                .unwrap(),
        );
        assert!(!secure.allows(&h, &Method::POST));
        h.insert(
            header::COOKIE,
            "__Host-minihabits_session=first".parse().unwrap(),
        );
        assert!(secure.allows(&h, &Method::POST));
        h.append(
            header::COOKIE,
            "__Host-minihabits_session=second".parse().unwrap(),
        );
        assert!(!secure.allows(&h, &Method::POST));
    }
}

//! Which requests pass under authentication: trust in the connection, credentials, origins, preflights and Go's
//! refusals.

use super::{AuthLease, Decision, Store, Via, protect};
use crate::{
    app::{Endpoint, finalize::Access, finalize::close, query, response},
    config,
    peer::{Address, Peer},
};
use graphite_meter_proto::{
    origin::{Origin, Scheme},
    route::{Kind, Route},
};
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};

/// Fields a request may carry at most once.
const SINGLE: [&str; 6] = [
    "authorization",
    "origin",
    "sec-fetch-site",
    "x-csrf-token",
    "access-control-request-method",
    "access-control-request-headers",
];
pub(super) const SESSION_COOKIE: &str = "__Host-gm_session";
const BROWSER_TOKEN: &str = "/auth/browser/token";

/// How far a request's connection is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Trust {
    /// TLS naming another hostname than the public origin's.
    Foreign,
    Insecure,
    /// HTTPS on the public hostname.
    Secure,
    /// HTTPS at the public origin itself.
    Canonical,
}

/// The rules of an enabled authentication.
#[derive(Debug)]
pub struct Policy {
    pub(super) public: Origin,
    /// The public origin as browsers send it in `Origin`.
    pub(super) origin: HeaderValue,
    login: HeaderValue,
    password: bool,
    oidc: bool,
}

impl Policy {
    pub(super) fn new(config: &config::Auth) -> Self {
        let origin = config.public_origin.to_string();
        let login = HeaderValue::from_str(&format!("{origin}/login")).expect("origins are ASCII");
        let origin = HeaderValue::from_str(&origin).expect("origins are ASCII");
        let (password, oidc) = (config.methods.password().is_some(), config.methods.oidc().is_some());
        Self {
            public: config.public_origin.clone(),
            origin,
            login,
            password,
            oidc,
        }
    }

    /// Go's boundary: ambiguous fields, trust, preflights, the controller's routes, then credentials and origins.
    pub(super) fn authorize<B>(
        &self,
        store: &Store,
        request: &Request<B>,
        endpoint: Endpoint,
        peer: &Peer,
    ) -> Decision {
        let headers = request.headers();
        if SINGLE.iter().any(|name| headers.get_all(*name).iter().nth(1).is_some()) {
            return self.forbidden(false);
        }
        let trust = self.trust(request, endpoint, peer);
        let secure = trust >= Trust::Secure;
        let (method, path) = (request.method(), request.uri().path());
        let route = Route::from_path(path);
        if trust == Trust::Foreign {
            return self.required(request, endpoint, false);
        }
        if method == Method::OPTIONS && (route.is_some() || path == BROWSER_TOKEN) {
            return self.preflight(headers, route, trust);
        }
        let controller = path == "/login" || path.starts_with("/auth/");
        if controller && (!endpoint.ui() || trust != Trust::Canonical) {
            return self.forbidden(secure);
        }
        if endpoint.ui() && self.public(method, path) {
            return match trust {
                Trust::Canonical => Decision::Allow(None),
                _ => self.required(request, endpoint, secure),
            };
        }
        if !secure {
            return self.required(request, endpoint, false);
        }
        let session = endpoint == Endpoint::Quic && route.is_some_and(|route| route.kind() == Kind::WebTransport);
        let lease = match method == Method::CONNECT && session {
            true => {
                // A CONNECT spends its ticket even when a bearer grant authenticates it.
                let ticket =
                    self.redeem(store, request, &query::get(request.uri().query(), "token").unwrap_or_default());
                self.bearer(store, headers).or(ticket)
            }
            false => self.authenticate(store, request, route),
        };
        let Some(lease) = lease else {
            return self.required(request, endpoint, true);
        };
        let misplaced = match lease.via() {
            Via::Bearer(browser) => route.is_none() || browser.is_some() && route == Some(Route::Servers),
            Via::Cookie => false,
        };
        if misplaced || !self.valid_origin(store, request, route, &lease) {
            return self.forbidden(true);
        }
        Decision::Allow(Some(lease))
    }

    /// Who may read a signed-in request's answer: a browser grant's origin, or the public origin with its cookie.
    pub(super) fn access(&self, lease: &AuthLease, headers: &HeaderMap) -> Option<Access> {
        if let Some(browser) = lease.browser() {
            return Some(Access::Bearer(browser.clone()));
        }
        (headers.get(header::ORIGIN) == Some(&self.origin)).then(|| Access::Cookie(self.origin.clone()))
    }

    fn trust<B>(&self, request: &Request<B>, endpoint: Endpoint, peer: &Peer) -> Trust {
        let authority = self.origin.to_str().unwrap_or_default().trim_start_matches("https://");
        if endpoint != Endpoint::H1 {
            let named = request_authority(request).unwrap_or_default();
            let host = Origin::parse(&format!("https://{named}"))
                .ok()
                .map(|origin| origin.host);
            return match (host.as_ref() == Some(&self.public.host), same_host(named, authority)) {
                (false, _) => Trust::Foreign,
                (true, false) => Trust::Secure,
                (true, true) => Trust::Canonical,
            };
        }
        let headers = request.headers();
        let forwarded = !matches!(peer.address(), Address::Socket(_))
            && single(headers, "x-forwarded-proto") == Some("https")
            && single(headers, "x-forwarded-host").is_some_and(|host| same_host(host, authority));
        if forwarded { Trust::Canonical } else { Trust::Insecure }
    }

    /// A socket ticket on a WebSocket route, else a bearer grant once `Authorization` is present, else the cookie.
    fn authenticate<B>(&self, store: &Store, request: &Request<B>, route: Option<Route>) -> Option<AuthLease> {
        let headers = request.headers();
        if route.is_some_and(|route| route.kind() == Kind::WebSocket)
            && let Some(token) = query::get(request.uri().query(), "token")
        {
            return self.redeem(store, request, &token);
        }
        if headers.contains_key(header::AUTHORIZATION) {
            return self.bearer(store, headers);
        }
        store.cookie(cookie(headers, SESSION_COOKIE)?)
    }

    fn bearer(&self, store: &Store, headers: &HeaderMap) -> Option<AuthLease> {
        let credentials = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
        store.bearer(credentials.strip_prefix("Bearer ")?)
    }

    /// Spends the ticket `token` presented at this request's HTTPS origin and path from its `Origin`.
    fn redeem<B>(&self, store: &Store, request: &Request<B>, token: &str) -> Option<AuthLease> {
        let origin = request_authority(request).and_then(|named| Origin::parse(&format!("https://{named}")).ok());
        let target = origin
            .map(|origin| format!("{origin}{}", request.uri().path()))
            .unwrap_or_default();
        store.redeem(token, &target, request.headers().get(header::ORIGIN))
    }

    /// Go's origin rules: a browser grant serves only its origin's measurement routes; other requests come from the
    /// public origin or none, and a cookie's unsafe measurement requests prove their CSRF token.
    fn valid_origin<B>(&self, store: &Store, request: &Request<B>, route: Option<Route>, lease: &AuthLease) -> bool {
        let headers = request.headers();
        let Some(origin) = text(headers, "origin") else { return false };
        let public = self.origin.to_str().unwrap_or_default();
        if let Some(browser) = lease.browser() {
            return origin.as_bytes() == browser.as_bytes() && route.is_some_and(|route| route != Route::Servers);
        }
        if !origin.is_empty() && origin != public {
            return false;
        }
        if lease.via() != &Via::Cookie {
            return true;
        }
        let ours = origin == public;
        match text(headers, "sec-fetch-site") {
            Some("" | "same-origin" | "none") => {}
            Some("same-site") if ours => {}
            _ => return false,
        }
        let read = matches!(*request.method(), Method::GET | Method::HEAD);
        let same_origin = text(headers, "sec-fetch-site") == Some("same-origin");
        if route.is_some() && read && !ours && !same_origin || route == Some(Route::Ping) && !ours {
            return false;
        }
        if read || request.method() == Method::OPTIONS {
            return true;
        }
        let proof = || text(headers, "x-csrf-token").is_some_and(|proof| store.csrf(lease.login(), proof));
        ours && (route.is_none() || proof())
    }

    /// Go's preflights: the browser token exchange, a browser grant's measurement routes, else the public origin's.
    fn preflight(&self, headers: &HeaderMap, route: Option<Route>, trust: Trust) -> Decision {
        let secure = trust >= Trust::Secure;
        let origin = headers.get(header::ORIGIN).filter(|origin| browser_origin(origin));
        let method = text(headers, "access-control-request-method").unwrap_or_default();
        let allowed = route.is_some_and(|route| route.methods().contains(&method));
        let mut answer = response::empty(StatusCode::NO_CONTENT);
        let answered = answer.headers_mut();
        match (route, origin) {
            (None, Some(origin))
                if trust == Trust::Canonical && method == "POST" && requested(headers, &[]).is_some() =>
            {
                Access::Bearer(origin.clone()).apply(answered);
                answered.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("POST"));
                answered.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("Content-Type"));
                answered.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("7200"));
            }
            (Some(route), Some(origin)) if secure && *origin != self.origin && route != Route::Servers => {
                let names = requested(headers, &["authorization"]);
                if !allowed || !names.is_some_and(|names| names.contains(&"authorization".into())) {
                    return self.forbidden(secure);
                }
                Access::Bearer(origin.clone()).apply_measurement(answered);
            }
            (Some(_), _) if secure && headers.get(header::ORIGIN) == Some(&self.origin) && allowed => {
                if requested(headers, &["authorization", "x-csrf-token"]).is_none() {
                    return self.forbidden(secure);
                }
                Access::Cookie(self.origin.clone()).apply_measurement(answered);
            }
            _ => return self.forbidden(secure),
        }
        crate::app::finalize::harden(answered, secure);
        Decision::Answer(answer)
    }

    /// Whether `method` reaches `path` without a session: the sign-in pages and their fonts, and the token
    /// exchanges.
    fn public(&self, method: &Method, path: &str) -> bool {
        const SANS: &str = "/fonts/ibm-plex-sans-var-latin1.woff2";
        const MONO: &str = "/fonts/ibm-plex-mono-600-latin1.woff2";
        match (method.as_str(), path) {
            ("GET", "/login" | "/auth/cli" | "/auth/browser") | ("POST", "/auth/cli/token" | BROWSER_TOKEN) => true,
            ("GET" | "HEAD", SANS | MONO) => true,
            ("POST", "/auth/password") => self.password,
            ("POST", "/auth/oidc/start") | ("GET", "/auth/oidc/callback") => self.oidc,
            _ => false,
        }
    }

    fn forbidden(&self, secure: bool) -> Decision {
        let mut answer = response::empty(StatusCode::FORBIDDEN);
        protect(answer.headers_mut(), secure);
        Decision::Answer(answer)
    }

    /// Go's sign-in-required answer, readable by the public origin or a browser origin measuring; the app root
    /// redirects to the sign-in page instead.
    fn required<B>(&self, request: &Request<B>, endpoint: Endpoint, secure: bool) -> Decision {
        let root = endpoint.ui() && request.method() == Method::GET && request.uri().path() == "/";
        let mut answer = match root {
            true => response::redirect(request.method(), StatusCode::TEMPORARY_REDIRECT, &self.login),
            false => response::empty(StatusCode::FORBIDDEN),
        };
        let headers = answer.headers_mut();
        protect(headers, secure);
        let origin = request.headers().get(header::ORIGIN);
        if origin == Some(&self.origin) {
            Access::Cookie(self.origin.clone()).apply(headers);
        } else if let Some(origin) = origin.filter(|origin| browser_origin(origin))
            && Route::from_path(request.uri().path()).is_some()
        {
            Access::Bearer(origin.clone()).apply(headers);
        }
        headers.insert("graphite-meter-auth", HeaderValue::from_static("required"));
        headers.insert("graphite-meter-browser-auth", HeaderValue::from_static("1"));
        headers.insert("graphite-meter-auth-url", self.login.clone());
        close(headers, request.version());
        Decision::Answer(answer)
    }
}

/// The host and port a request names: its URI's authority, else its Host header.
fn request_authority<B>(request: &Request<B>) -> Option<&str> {
    match request.uri().authority() {
        Some(authority) => Some(authority.as_str()),
        None => request.headers().get(header::HOST)?.to_str().ok(),
    }
}

/// A field's value as Go's `Header.Get` reads it: empty when absent, `None` when it is no text.
fn text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).map_or(Some(""), |value| value.to_str().ok())
}

/// A field present once with one comma-free value.
fn single<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next().filter(|_| values.next().is_none())?.to_str().ok()?;
    (!value.contains(',')).then(|| value.trim())
}

fn same_host(first: &str, second: &str) -> bool {
    first
        .trim_end_matches('.')
        .eq_ignore_ascii_case(second.trim_end_matches('.'))
}

/// An exact canonical HTTPS origin, as a browser grant's audience must be.
fn browser_origin(origin: &HeaderValue) -> bool {
    let text = origin.to_str().unwrap_or_default();
    Origin::parse(text).is_ok_and(|parsed| parsed.scheme == Scheme::Https && parsed.to_string() == text)
}

/// The preflight's requested headers in lower case, when each is `Content-Type` or one of `allowed`.
fn requested(headers: &HeaderMap, allowed: &[&str]) -> Option<Vec<String>> {
    let names = text(headers, "access-control-request-headers")
        .unwrap_or_default()
        .split(',');
    let names = names
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty());
    let known = |name: &str| name == "content-type" || allowed.contains(&name);
    names.map(|name| known(&name).then_some(name)).collect()
}

/// Go's `CookiesNamed` for one cookie: invalid pairs are skipped, and two valid ones are ambiguous.
pub(super) fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let values = headers.get_all(header::COOKIE).iter();
    let pairs: Vec<_> = values
        .flat_map(|value| value.as_bytes().split(|&byte| byte == b';'))
        .collect();
    if pairs.len() > 3000 {
        return None;
    }
    let mut found = None;
    for pair in pairs {
        let pair = pair.trim_ascii();
        let (key, value) = match pair.iter().position(|&byte| byte == b'=') {
            Some(at) => (pair[..at].trim_ascii(), &pair[at + 1..]),
            None => (pair, &b""[..]),
        };
        let value = value
            .strip_prefix(b"\"")
            .and_then(|value| value.strip_suffix(b"\""))
            .unwrap_or(value);
        let valid = value
            .iter()
            .all(|byte| (0x20..0x7f).contains(byte) && !b"\";\\".contains(byte));
        if key == name.as_bytes() && valid && found.replace(value).is_some() {
            return None;
        }
    }
    found.and_then(|value| std::str::from_utf8(value).ok())
}

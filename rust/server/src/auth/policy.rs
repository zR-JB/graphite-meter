//! Decides which requests pass under authentication by connection trust, credentials, origins, preflights, refusals.

use super::{AuthLease, Decision, Store, Via, protect};
use crate::headers::{
    GRAPHITE_METER_AUTH, GRAPHITE_METER_AUTH_URL, GRAPHITE_METER_BROWSER_AUTH, SEC_FETCH_SITE, X_CSRF_TOKEN,
    X_FORWARDED_HOST,
};
use crate::{
    app::{Endpoint, authority, finalize::Access, finalize::close, query, response},
    config,
    peer::{Peer, single},
};
use graphite_meter_proto::{
    origin::{Origin, Scheme},
    route::{Kind, Route},
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header, uri::Authority};

/// A request may carry these fields at most once.
const SINGLE: [HeaderName; 6] = [
    header::AUTHORIZATION,
    header::ORIGIN,
    SEC_FETCH_SITE,
    X_CSRF_TOKEN,
    header::ACCESS_CONTROL_REQUEST_METHOD,
    header::ACCESS_CONTROL_REQUEST_HEADERS,
];

/// Says when a controller route serves without a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Open {
    Always,
    /// The route serves while password sign-in is enabled.
    Password,
    /// The route serves while OIDC sign-in is enabled.
    Oidc,
    Never,
}

graphite_meter_proto::table! {
    /// Lists the controller's routes with method, path, and when each serves without a session.
    pub(super) enum AuthRoute {
        method.0: &'static str,
        path.1: &'static str,
        open.2: Open,
    } {
        LoginPage => ("GET", "/login", Open::Always),
        Password => ("POST", "/auth/password", Open::Password),
        OidcStart => ("POST", "/auth/oidc/start", Open::Oidc),
        OidcCallback => ("GET", "/auth/oidc/callback", Open::Oidc),
        Session => ("GET", "/auth/session", Open::Never),
        CliPage => ("GET", "/auth/cli", Open::Always),
        BrowserPage => ("GET", "/auth/browser", Open::Always),
        CliApprove => ("POST", "/auth/cli/approve", Open::Never),
        BrowserApprove => ("POST", "/auth/browser/approve", Open::Never),
        CliToken => ("POST", "/auth/cli/token", Open::Always),
        BrowserToken => ("POST", "/auth/browser/token", Open::Always),
        Logout => ("POST", "/auth/logout", Open::Never),
    }
}

impl AuthRoute {
    /// Finds the route `method` and `path` name exactly.
    pub(super) fn find(method: &str, path: &str) -> Option<Self> {
        Self::find_by(Self::path, path).filter(|route| route.method() == method)
    }
}

graphite_meter_proto::table! {
    /// A cookie the controller sets is host-only and secure, HttpOnly unless scripts read it, strictly same-site
    /// unless a cross-site redirect must carry it.
    pub(super) enum Cookie {
        name.0: &'static str,
        script.1: bool,
        same_site.2: &'static str,
    } {
        Session => ("__Host-gm_session", false, "Strict"),
        /// The cookie holds a sign-in form's token.
        Login => ("__Host-gm_login", false, "Strict"),
        /// The cookie holds the login's CSRF token, which the app sends back in a header.
        Csrf => ("__Host-gm_csrf", true, "Strict"),
        /// The cookie carries a browser's password attempt budget.
        Device => ("__Host-gm_device", false, "Strict"),
        /// The cookie names the OIDC transaction and rides the provider's cross-site redirect back.
        Transaction => ("__Host-gm_oidc", false, "Lax"),
    }
}

/// Says how far a request's connection is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Trust {
    /// TLS names another hostname than the public origin's.
    Foreign,
    Insecure,
    /// HTTPS reaches the public hostname.
    Secure,
    /// HTTPS reaches the public origin itself.
    Canonical,
}

/// Holds the rules of an enabled authentication.
#[derive(Debug)]
pub struct Policy {
    pub(super) public: Origin,
    /// Browsers send the public origin like this in `Origin`.
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

    /// Checks ambiguous fields, trust, preflights, the controller's routes, then credentials and origins.
    pub(super) fn authorize<B>(
        &self,
        store: &Store,
        request: &Request<B>,
        endpoint: Endpoint,
        peer: &Peer,
    ) -> Decision {
        let headers = request.headers();
        if SINGLE.iter().any(|name| headers.get_all(name).iter().nth(1).is_some()) {
            return self.forbidden(false);
        }
        let trust = self.trust(request, endpoint, peer);
        let secure = trust >= Trust::Secure;
        let (method, path) = (request.method(), request.uri().path());
        let route = Route::find_by(Route::path, path);
        if trust == Trust::Foreign {
            return self.required(request, endpoint, false);
        }
        if method == Method::OPTIONS && (route.is_some() || path == AuthRoute::BrowserToken.path()) {
            return self.preflight(headers, route, trust);
        }
        let controller = path == "/login" || path.starts_with("/auth/");
        if controller && (!endpoint.ui() || trust != Trust::Canonical) {
            return self.forbidden(secure);
        }
        if endpoint.ui() && self.public(method, path) {
            return match trust {
                Trust::Canonical => Ok(None),
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
        Ok(Some(lease))
    }

    /// A signed-in request's answer is readable by a browser grant's origin, or by the public origin with its cookie.
    pub(super) fn access(&self, lease: &AuthLease, headers: &HeaderMap) -> Option<Access> {
        if let Some(browser) = lease.browser() {
            return Some(Access::Bearer(browser.header().clone()));
        }
        (headers.get(header::ORIGIN) == Some(&self.origin)).then(|| Access::Cookie(self.origin.clone()))
    }

    fn trust<B>(&self, request: &Request<B>, endpoint: Endpoint, peer: &Peer) -> Trust {
        let public = self.origin.to_str().unwrap_or_default().trim_start_matches("https://");
        if endpoint != Endpoint::H1 {
            let named = authority(request);
            let named = named.as_ref().map_or("", Authority::as_str);
            let host = Origin::parse(&format!("https://{named}"))
                .ok()
                .map(|origin| origin.host);
            return match (host.as_ref() == Some(&self.public.host), same_host(named, public)) {
                (false, _) => Trust::Foreign,
                (true, false) => Trust::Secure,
                (true, true) => Trust::Canonical,
            };
        }
        let headers = request.headers();
        let forwarded = peer.forwarded_tls(headers)
            && single(headers, &X_FORWARDED_HOST).is_some_and(|host| same_host(host, public));
        if forwarded { Trust::Canonical } else { Trust::Insecure }
    }

    /// Reads a socket ticket on a WebSocket route, a bearer grant once `Authorization` is present, else the cookie.
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
        cookie_lease(store, headers)
    }

    fn bearer(&self, store: &Store, headers: &HeaderMap) -> Option<AuthLease> {
        let credentials = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
        store.bearer(credentials.strip_prefix("Bearer ")?)
    }

    /// Spends the ticket `token` presented at this request's HTTPS origin and path from its `Origin`.
    fn redeem<B>(&self, store: &Store, request: &Request<B>, token: &str) -> Option<AuthLease> {
        let origin = authority(request).and_then(|named| Origin::parse(&format!("https://{named}")).ok());
        let target = origin
            .map(|origin| format!("{origin}{}", request.uri().path()))
            .unwrap_or_default();
        store.redeem(token, &target, request.headers().get(header::ORIGIN))
    }

    /// Browser grants serve only their origin's measurement routes, and unsafe cookie requests prove CSRF.
    fn valid_origin<B>(&self, store: &Store, request: &Request<B>, route: Option<Route>, lease: &AuthLease) -> bool {
        let headers = request.headers();
        let Some(origin) = text(headers, &header::ORIGIN) else { return false };
        let public = self.origin.to_str().unwrap_or_default();
        if let Some(browser) = lease.browser() {
            return origin.as_bytes() == browser.header().as_bytes()
                && route.is_some_and(|route| route != Route::Servers);
        }
        if !origin.is_empty() && origin != public {
            return false;
        }
        if lease.via() != &Via::Cookie {
            return true;
        }
        let ours = origin == public;
        match text(headers, &SEC_FETCH_SITE) {
            Some("" | "same-origin" | "none") => {}
            Some("same-site") if ours => {}
            _ => return false,
        }
        let read = matches!(*request.method(), Method::GET | Method::HEAD);
        let same_origin = text(headers, &SEC_FETCH_SITE) == Some("same-origin");
        if route.is_some() && read && !ours && !same_origin || route == Some(Route::Ping) && !ours {
            return false;
        }
        if read || request.method() == Method::OPTIONS {
            return true;
        }
        let proof = || text(headers, &X_CSRF_TOKEN).is_some_and(|proof| store.csrf(lease.login(), proof));
        ours && (route.is_none() || proof())
    }

    /// Answers preflights for the browser token exchange, a browser grant's measurement routes or the public origin's.
    fn preflight(&self, headers: &HeaderMap, route: Option<Route>, trust: Trust) -> Decision {
        let secure = trust >= Trust::Secure;
        let origin = headers.get(header::ORIGIN).and_then(BrowserOrigin::parse);
        let method = text(headers, &header::ACCESS_CONTROL_REQUEST_METHOD).unwrap_or_default();
        let allowed = route.is_some_and(|route| route.methods().contains(&method));
        let mut answer = response::empty(StatusCode::NO_CONTENT);
        let answered = answer.headers_mut();
        match (route, origin) {
            (None, Some(origin))
                if trust == Trust::Canonical && method == "POST" && requested(headers, &[]).is_some() =>
            {
                Access::Bearer(origin.header().clone()).apply(answered);
                answered.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("POST"));
                answered.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("Content-Type"));
                answered.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("7200"));
            }
            (Some(route), Some(origin)) if secure && *origin.header() != self.origin && route != Route::Servers => {
                let names = requested(headers, &["authorization"]);
                if !allowed || !names.is_some_and(|names| names.contains(&"authorization".into())) {
                    return self.forbidden(secure);
                }
                Access::Bearer(origin.header().clone()).apply_measurement(answered);
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
        Err(answer)
    }

    /// Says whether `method` reaches `path` without a session, as sign-in pages, their fonts and token exchanges do.
    fn public(&self, method: &Method, path: &str) -> bool {
        const SANS: &str = "/fonts/ibm-plex-sans-var-latin1.woff2";
        const MONO: &str = "/fonts/ibm-plex-mono-600-latin1.woff2";
        if matches!(*method, Method::GET | Method::HEAD) && matches!(path, SANS | MONO) {
            return true;
        }
        AuthRoute::find(method.as_str(), path).is_some_and(|route| match route.open() {
            Open::Always => true,
            Open::Password => self.password,
            Open::Oidc => self.oidc,
            Open::Never => false,
        })
    }

    fn forbidden(&self, secure: bool) -> Decision {
        let mut answer = response::empty(StatusCode::FORBIDDEN);
        protect(answer.headers_mut(), secure);
        Err(answer)
    }

    /// Requires sign-in, readable by the public or a measuring browser origin. The app root redirects.
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
        } else if let Some(origin) = origin.and_then(BrowserOrigin::parse)
            && Route::find_by(Route::path, request.uri().path()).is_some()
        {
            Access::Bearer(origin.header().clone()).apply(headers);
        }
        headers.insert(GRAPHITE_METER_AUTH, HeaderValue::from_static("required"));
        headers.insert(GRAPHITE_METER_BROWSER_AUTH, HeaderValue::from_static("1"));
        headers.insert(GRAPHITE_METER_AUTH_URL, self.login.clone());
        close(headers, request.version());
        Err(answer)
    }
}

/// Gives a field's first value, empty when absent and `None` when it is no text.
fn text<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    headers.get(name).map_or(Some(""), |value| value.to_str().ok())
}

fn same_host(first: &str, second: &str) -> bool {
    first
        .trim_end_matches('.')
        .eq_ignore_ascii_case(second.trim_end_matches('.'))
}

/// Holds an exact canonical HTTPS origin, as a browser grant's audience must be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserOrigin(HeaderValue);

impl BrowserOrigin {
    pub fn parse(origin: &HeaderValue) -> Option<Self> {
        let text = origin.to_str().ok()?;
        let parsed = Origin::parse(text).ok()?;
        (parsed.scheme == Scheme::Https && parsed.to_string() == text).then(|| Self(origin.clone()))
    }

    pub fn header(&self) -> &HeaderValue {
        &self.0
    }
}

/// Lists the preflight's requested headers in lower case, when each is `Content-Type` or one of `allowed`.
fn requested(headers: &HeaderMap, allowed: &[&str]) -> Option<Vec<String>> {
    let names = text(headers, &header::ACCESS_CONTROL_REQUEST_HEADERS)
        .unwrap_or_default()
        .split(',');
    let names = names
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty());
    let known = |name: &str| name == "content-type" || allowed.contains(&name);
    names.map(|name| known(&name).then_some(name)).collect()
}

/// Finds the lease of the login whose session cookie `headers` carry.
pub(super) fn cookie_lease(store: &Store, headers: &HeaderMap) -> Option<AuthLease> {
    store.cookie(cookie(headers, Cookie::Session)?)
}

/// Reads the cookie's value. Invalid pairs are skipped, and two valid ones are ambiguous.
pub(super) fn cookie(headers: &HeaderMap, cookie: Cookie) -> Option<&str> {
    let name = cookie.name();
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

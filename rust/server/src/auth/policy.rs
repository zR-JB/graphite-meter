//! Which requests pass under authentication: trust in the connection, credentials, origins, preflights and refusals.

use super::{AuthLease, Decision, Store, Via, protect};
use crate::headers::{
    GRAPHITE_METER_AUTH_URL, GRAPHITE_METER_BROWSER_AUTH, SEC_FETCH_SITE, X_CSRF_TOKEN,
    X_FORWARDED_HOST,
};
use crate::{
    app::{Endpoint, authority, finalize::Access, finalize::close, query, response},
    config,
    peer::{Peer, single},
};
use graphite_meter_proto::{
    origin::{Origin, Scheme},
    refusal::SIGN_IN_HEADER,
    route::{AuthRoute, Kind, Route},
};
use http::{
    HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header, uri::Authority,
};

const SINGLE: [HeaderName; 6] = [
    header::AUTHORIZATION,
    header::ORIGIN,
    SEC_FETCH_SITE,
    X_CSRF_TOKEN,
    header::ACCESS_CONTROL_REQUEST_METHOD,
    header::ACCESS_CONTROL_REQUEST_HEADERS,
];

pub(super) fn auth_route(method: &str, path: &str) -> Option<AuthRoute> {
    AuthRoute::find_by(AuthRoute::path, path).filter(|route| route.method() == method)
}

/// Whether `path` lies in the sign-in controller's namespace, a route there or not.
pub(super) fn controls(path: &str) -> bool {
    path == AuthRoute::Login.path() || path.starts_with("/auth/")
}

graphite_meter_proto::table! {
    /// Host-only and secure; HttpOnly unless scripts read it, Strict unless a cross-site redirect must carry it.
    pub(super) enum Cookie {
        name.0: &'static str,
        script.1: bool,
        same_site.2: &'static str,
    } {
        Session => ("__Host-gm_session", false, "Strict"),
        Login => ("__Host-gm_login", false, "Strict"),
        /// The login's CSRF token, which the app sends back in a header.
        Csrf => ("__Host-gm_csrf", true, "Strict"),
        Device => ("__Host-gm_device", false, "Strict"),
        /// The OIDC transaction, carried on the provider's cross-site redirect back.
        Transaction => ("__Host-gm_oidc", false, "Lax"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Trust {
    /// TLS naming another hostname than the public origin's.
    Foreign,
    Insecure,
    Secure,
    Canonical,
}

#[derive(Debug)]
pub struct Policy {
    pub(super) public: Origin,
    pub(super) origin: HeaderValue,
    login: HeaderValue,
    password: bool,
    oidc: bool,
}

impl Policy {
    pub(super) fn new(config: &config::Auth) -> Self {
        let origin = config.public_origin.to_string();
        let login = HeaderValue::from_str(&format!("{origin}{}", AuthRoute::Login.path()))
            .expect("origins are ASCII");
        let origin = HeaderValue::from_str(&origin).expect("origins are ASCII");
        let (password, oidc) = (
            config.methods.password.is_some(),
            config.methods.oidc.is_some(),
        );
        Self {
            public: config.public_origin.clone(),
            origin,
            login,
            password,
            oidc,
        }
    }

    /// The boundary: ambiguous fields, trust, preflights, the controller's routes, then credentials and origins.
    pub(super) fn authorize<B>(
        &self,
        store: &Store,
        request: &Request<B>,
        endpoint: Endpoint,
        peer: &Peer,
    ) -> Decision {
        let headers = request.headers();
        if SINGLE
            .iter()
            .any(|name| headers.get_all(name).iter().nth(1).is_some())
        {
            return self.forbidden(false);
        }
        let trust = self.trust(request, endpoint, peer);
        let secure = trust >= Trust::Secure;
        let (method, path) = (request.method(), request.uri().path());
        let route = Route::find_by(Route::path, path);
        if trust == Trust::Foreign {
            return self.required(request, endpoint, false);
        }
        if method == Method::OPTIONS && (route.is_some() || path == AuthRoute::BrowserToken.path())
        {
            return self.preflight(headers, route, trust);
        }
        if controls(path) && (!endpoint.ui() || trust != Trust::Canonical) {
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
        let session = endpoint == Endpoint::Quic
            && route.is_some_and(|route| route.kind() == Kind::WebTransport);
        let lease = match method == Method::CONNECT && session {
            true => {
                // A CONNECT spends its ticket even when a bearer grant authenticates it.
                let ticket = self.redeem(
                    store,
                    request,
                    &query::get(request.uri().query(), "token").unwrap_or_default(),
                );
                self.bearer(store, headers).or(ticket)
            }
            false => self.authenticate(store, request, route),
        };
        let Some(lease) = lease else {
            return self.required(request, endpoint, true);
        };
        let misplaced = match lease.via() {
            Via::Bearer(browser) => {
                route.is_none() || browser.is_some() && route == Some(Route::Servers)
            }
            Via::Cookie => false,
        };
        if misplaced || !self.valid_origin(store, request, route, &lease) {
            return self.forbidden(true);
        }
        Ok(Some(lease))
    }

    /// Who may read a signed-in request's answer: a browser grant's origin, or the public origin with its cookie.
    pub(super) fn access(&self, lease: &AuthLease, headers: &HeaderMap) -> Option<Access> {
        if let Some(browser) = lease.browser() {
            return Some(Access::Bearer(browser.header().clone()));
        }
        (headers.get(header::ORIGIN) == Some(&self.origin))
            .then(|| Access::Cookie(self.origin.clone()))
    }

    fn trust<B>(&self, request: &Request<B>, endpoint: Endpoint, peer: &Peer) -> Trust {
        let public = self
            .origin
            .to_str()
            .unwrap_or_default()
            .trim_start_matches("https://");
        if endpoint != Endpoint::H1 {
            let named = authority(request);
            let named = named.as_ref().map_or("", Authority::as_str);
            let host = Origin::parse(&format!("https://{named}"))
                .ok()
                .map(|origin| origin.host);
            return match (
                host.as_ref() == Some(&self.public.host),
                same_host(named, public),
            ) {
                (false, _) => Trust::Foreign,
                (true, false) => Trust::Secure,
                (true, true) => Trust::Canonical,
            };
        }
        let headers = request.headers();
        let forwarded = peer.forwarded_tls(headers)
            && single(headers, &X_FORWARDED_HOST).is_some_and(|host| same_host(host, public));
        if forwarded {
            Trust::Canonical
        } else {
            Trust::Insecure
        }
    }

    /// A socket ticket on a WebSocket route, else a bearer grant once `Authorization` is present, else the cookie.
    fn authenticate<B>(
        &self,
        store: &Store,
        request: &Request<B>,
        route: Option<Route>,
    ) -> Option<AuthLease> {
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
        let origin =
            authority(request).and_then(|named| Origin::parse(&format!("https://{named}")).ok());
        let target = origin
            .map(|origin| format!("{origin}{}", request.uri().path()))
            .unwrap_or_default();
        store.redeem(token, &target, request.headers().get(header::ORIGIN))
    }

    /// Origin rules: browser grants serve only their origin's measurement routes; unsafe cookie requests prove CSRF.
    fn valid_origin<B>(
        &self,
        store: &Store,
        request: &Request<B>,
        route: Option<Route>,
        lease: &AuthLease,
    ) -> bool {
        let headers = request.headers();
        let Some(origin) = text(headers, &header::ORIGIN) else {
            return false;
        };
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
        let site = text(headers, &SEC_FETCH_SITE);
        match site {
            Some("" | "same-origin" | "none") => {}
            Some("same-site") if ours => {}
            _ => return false,
        }
        let read = matches!(*request.method(), Method::GET | Method::HEAD);
        let same_origin = site == Some("same-origin");
        if route.is_some() && read && !ours && !same_origin || route == Some(Route::Ping) && !ours {
            return false;
        }
        if read || request.method() == Method::OPTIONS {
            return true;
        }
        let proof =
            || text(headers, &X_CSRF_TOKEN).is_some_and(|proof| store.csrf(lease.login(), proof));
        ours && (route.is_none() || proof())
    }

    /// Preflights: the browser token exchange, a browser grant's measurement routes, else the public origin's.
    fn preflight(&self, headers: &HeaderMap, route: Option<Route>, trust: Trust) -> Decision {
        let secure = trust >= Trust::Secure;
        let origin = headers.get(header::ORIGIN).and_then(BrowserOrigin::parse);
        let method = text(headers, &header::ACCESS_CONTROL_REQUEST_METHOD).unwrap_or_default();
        let allowed = route.is_some_and(|route| route.methods().contains(&method));
        let mut answer = response::empty(StatusCode::NO_CONTENT);
        let answered = answer.headers_mut();
        match (route, origin) {
            (None, Some(origin))
                if trust == Trust::Canonical
                    && method == "POST"
                    && requested(headers, &[]).is_some() =>
            {
                Access::Bearer(origin.header().clone()).apply_preflight(
                    answered,
                    "POST",
                    "Content-Type",
                );
            }
            (Some(route), Some(origin))
                if secure && *origin.header() != self.origin && route != Route::Servers =>
            {
                let names = requested(headers, &["authorization"]);
                if !allowed || !names.is_some_and(|names| names.contains(&"authorization".into())) {
                    return self.forbidden(secure);
                }
                Access::Bearer(origin.header().clone()).apply_measurement(answered);
            }
            (Some(_), _)
                if secure && headers.get(header::ORIGIN) == Some(&self.origin) && allowed =>
            {
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

    /// Whether `method` reaches `path` without a session: the sign-in pages and their fonts, and the token exchanges.
    fn public(&self, method: &Method, path: &str) -> bool {
        const SANS: &str = "/fonts/ibm-plex-sans-var-latin1.woff2";
        const MONO: &str = "/fonts/ibm-plex-mono-600-latin1.woff2";
        if matches!(*method, Method::GET | Method::HEAD) && matches!(path, SANS | MONO) {
            return true;
        }
        auth_route(method.as_str(), path).is_some_and(|route| match route {
            AuthRoute::Login
            | AuthRoute::Cli
            | AuthRoute::Browser
            | AuthRoute::CliToken
            | AuthRoute::BrowserToken => true,
            AuthRoute::Password => self.password,
            AuthRoute::OidcStart | AuthRoute::OidcCallback => self.oidc,
            AuthRoute::Session
            | AuthRoute::CliApprove
            | AuthRoute::BrowserApprove
            | AuthRoute::Logout => false,
        })
    }

    fn forbidden(&self, secure: bool) -> Decision {
        let mut answer = response::empty(StatusCode::FORBIDDEN);
        protect(answer.headers_mut(), secure, None);
        Err(answer)
    }

    /// The sign-in-required answer, readable by the public or a measuring browser origin; the app root redirects.
    fn required<B>(&self, request: &Request<B>, endpoint: Endpoint, secure: bool) -> Decision {
        let root = endpoint.ui() && request.method() == Method::GET && request.uri().path() == "/";
        let mut answer = match root {
            true => response::redirect(
                request.method(),
                StatusCode::TEMPORARY_REDIRECT,
                &self.login,
            ),
            false => response::empty(StatusCode::FORBIDDEN),
        };
        let headers = answer.headers_mut();
        protect(headers, secure, None);
        let origin = request.headers().get(header::ORIGIN);
        if origin == Some(&self.origin) {
            Access::Cookie(self.origin.clone()).apply(headers);
        } else if let Some(origin) = origin.and_then(BrowserOrigin::parse)
            && Route::find_by(Route::path, request.uri().path()).is_some()
        {
            Access::Bearer(origin.header().clone()).apply(headers);
        }
        headers.insert(SIGN_IN_HEADER, HeaderValue::from_static("required"));
        headers.insert(GRAPHITE_METER_BROWSER_AUTH, HeaderValue::from_static("1"));
        headers.insert(GRAPHITE_METER_AUTH_URL, self.login.clone());
        close(headers, request.version());
        Err(answer)
    }
}

/// A field's first value: empty when absent, `None` when it is no text.
fn text<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    headers
        .get(name)
        .map_or(Some(""), |value| value.to_str().ok())
}

fn same_host(first: &str, second: &str) -> bool {
    first
        .trim_end_matches('.')
        .eq_ignore_ascii_case(second.trim_end_matches('.'))
}

/// An exact canonical HTTPS origin, as a browser grant's audience must be.
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

/// The preflight's requested headers in lower case, when each is `Content-Type` or one of `allowed`.
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

pub(super) fn cookie_lease(store: &Store, headers: &HeaderMap) -> Option<AuthLease> {
    store.cookie(cookie(headers, Cookie::Session)?)
}

/// The cookie's value: invalid pairs are skipped, and two valid ones are ambiguous.
pub(super) fn cookie(headers: &HeaderMap, cookie: Cookie) -> Option<&str> {
    let name = cookie.name();
    let values = headers.get_all(header::COOKIE).iter();
    let pairs = values.flat_map(|value| value.as_bytes().split(|&byte| byte == b';'));
    let mut found = None;
    for (index, pair) in pairs.enumerate() {
        if index == 3000 {
            return None;
        }
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

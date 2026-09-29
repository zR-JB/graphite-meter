//! Request authorization decisions, separate from response bodies and transport IO.

use super::{
    AuthLease, SessionStore,
    route::{self as auth_route, AuthRoute},
};
use crate::{
    client_address::unique_header,
    config::{AuthMode, ConfigError},
    cors::{self, Access},
    http::response::query,
};
use graphite_meter_core::{
    origin::{canonical_origin, target_origin},
    route::{self, Kind, Route},
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, header};
use ipnet::IpNet;
use std::net::{IpAddr, SocketAddr};
use subtle::ConstantTimeEq;

#[derive(Clone, Copy, Default)]
pub struct Listener {
    pub ui: bool,
    pub webtransport: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trust {
    pub secure: bool,
    pub canonical: bool,
}

#[derive(Clone)]
pub enum Authorization {
    PublicAuth,
    Preflight(HeaderMap),
    Authenticated(AuthLease),
}

/// Facts supplied by the accepting listener, never by request headers.
#[derive(Clone, Copy)]
pub struct Connection {
    pub peer: SocketAddr,
    pub tls: bool,
    pub listener: Listener,
}

/// The permission travels with the exact method, route, authority and Origin
/// that were checked. Only the body may be transformed after authorization.
pub struct AuthorizedRequest<B> {
    request: Request<B>,
    authorization: Authorization,
    connection: Connection,
}

impl<B> AuthorizedRequest<B> {
    pub fn request(&self) -> &Request<B> {
        &self.request
    }
    pub fn authorization(&self) -> &Authorization {
        &self.authorization
    }
    pub fn connection(&self) -> Connection {
        self.connection
    }
    /// Collect or adapt a body without changing the authority or route checked
    /// by the policy. The caller supplies the size and time bounds.
    pub async fn try_map_body<C, E>(self, map: impl AsyncFnOnce(B) -> Result<C, E>) -> Result<AuthorizedRequest<C>, E> {
        let (parts, body) = self.request.into_parts();
        Ok(AuthorizedRequest {
            request: Request::from_parts(parts, map(body).await?),
            authorization: self.authorization,
            connection: self.connection,
        })
    }

    /// Only the transport dispatcher may split the checked request from its
    /// lease. It must retain that lease through the complete IO lifetime.
    pub(crate) fn into_parts(self) -> (Request<B>, Authorization) {
        (self.request, self.authorization)
    }
}

pub struct RejectedRequest<B> {
    request: Request<B>,
    reason: Refusal,
}

impl<B> RejectedRequest<B> {
    pub fn request(&self) -> &Request<B> {
        &self.request
    }
    pub fn reason(&self) -> Refusal {
        self.reason
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    AuthenticationRequired,
    Forbidden,
    /// A repeated security-relevant field, which Go's Enforce refuses before it trusts the connection.
    Ambiguous,
}

/// Construct only for an enabled, validated authentication configuration.
pub struct Policy {
    public: String,
    public_header: HeaderValue,
    hostname: String,
    authority: String,
    mode: AuthMode,
    trusted: Vec<IpNet>,
    sessions: SessionStore,
}

impl Policy {
    pub fn new(public: &str, mode: AuthMode, trusted: Vec<IpNet>, sessions: SessionStore) -> Result<Self, ConfigError> {
        let origin = target_origin(public)?.ok_or("authentication requires a public origin")?;
        if origin.scheme != "https" || canonical_origin(public)? != public {
            return Err("authentication requires a canonical HTTPS origin".into());
        }
        Ok(Self {
            public: public.into(),
            public_header: HeaderValue::from_str(public)?,
            hostname: origin.host,
            authority: public.strip_prefix("https://").expect("validated scheme").into(),
            mode,
            trusted,
            sessions,
        })
    }

    pub fn public_origin(&self) -> &str {
        &self.public
    }

    pub fn trust<B>(&self, request: &Request<B>, peer: SocketAddr, tls: bool) -> Trust {
        if tls {
            let authority = authority(request).unwrap_or_default();
            // Native TLS listeners may use a different port, but never another
            // hostname. Account pages additionally require the canonical port.
            let secure = target_origin(&format!("https://{authority}"))
                .ok()
                .flatten()
                .is_some_and(|origin| origin.host.eq_ignore_ascii_case(&self.hostname));
            return Trust {
                secure,
                canonical: secure && equal_host(authority, &self.authority),
            };
        }
        let forwarded = self.trusted_peer(peer.ip())
            && single_header(request.headers(), "x-forwarded-proto") == Some("https")
            && single_header(request.headers(), "x-forwarded-host")
                .is_some_and(|host| equal_host(host, &self.authority));
        Trust {
            secure: forwarded,
            canonical: forwarded,
        }
    }

    pub fn client_address(&self, headers: &HeaderMap, peer: SocketAddr) -> Option<IpAddr> {
        let client = crate::client_address::resolve(peer, headers, &self.trusted);
        client.usable.then_some(client.addr)
    }

    pub fn authorize<B>(
        &self,
        request: Request<B>,
        connection: Connection,
    ) -> Result<AuthorizedRequest<B>, Box<RejectedRequest<B>>> {
        match self.evaluate(&request, connection.peer, connection.tls, connection.listener) {
            Ok(authorization) => Ok(AuthorizedRequest {
                request,
                authorization,
                connection,
            }),
            Err(reason) => Err(Box::new(RejectedRequest { request, reason })),
        }
    }

    fn evaluate<B>(
        &self,
        request: &Request<B>,
        peer: SocketAddr,
        tls: bool,
        listener: Listener,
    ) -> Result<Authorization, Refusal> {
        // Go's Enforce refuses a repeated security-relevant field before anything else.
        if [
            header::AUTHORIZATION,
            header::ORIGIN,
            HeaderName::from_static("sec-fetch-site"),
            HeaderName::from_static("x-csrf-token"),
            header::ACCESS_CONTROL_REQUEST_METHOD,
            header::ACCESS_CONTROL_REQUEST_HEADERS,
        ]
        .iter()
        .any(|name| request.headers().get_all(name).iter().nth(1).is_some())
        {
            return Err(Refusal::Ambiguous);
        }
        let trust = self.trust(request, peer, tls);
        if tls && !trust.secure {
            return Err(Refusal::AuthenticationRequired);
        }
        let path = request.uri().path();
        let route = route::lookup(path);
        if request.method() == Method::OPTIONS {
            if trust.secure && trust.canonical && path == AuthRoute::BrowserToken.path() {
                return self.browser_preflight(request.headers()).map(Authorization::Preflight);
            }
            if route.is_some() {
                let access = cors::authenticated_preflight(&self.public_header, trust.secure, route, request.headers())
                    .ok_or(Refusal::Forbidden)?;
                let mut headers = HeaderMap::new();
                access.apply_measurement(&mut headers);
                return Ok(Authorization::Preflight(headers));
            }
        }
        if request.method() == Method::CONNECT
            && listener.webtransport
            && route.is_some_and(|route| route.kind() == Kind::WebTransport)
        {
            if !trust.secure {
                return Err(Refusal::AuthenticationRequired);
            }
            let ticket = query(request, "token");
            let bearer = self.bearer(request.headers());
            // A bearer-authenticated CONNECT still burns a supplied ticket.
            let redeemed = ticket.as_deref().and_then(|token| self.consume_ticket(request, token));
            let lease = bearer.or(redeemed).ok_or(Refusal::AuthenticationRequired)?;
            if !self.valid_origin(request, &lease) {
                return Err(Refusal::Forbidden);
            }
            return Ok(Authorization::Authenticated(lease));
        }
        if auth_route::claims(path) && (!listener.ui || !trust.canonical) {
            return Err(Refusal::Forbidden);
        }
        if listener.ui && AuthRoute::lookup(request.method(), path).is_some_and(|route| route.public(self.mode)) {
            return if trust.secure && trust.canonical {
                Ok(Authorization::PublicAuth)
            } else {
                Err(Refusal::AuthenticationRequired)
            };
        }
        if !trust.secure {
            return Err(Refusal::AuthenticationRequired);
        }
        let lease = self.authenticate(request).ok_or(Refusal::AuthenticationRequired)?;
        if lease.is_bearer() && (route.is_none() || lease.browser_origin().is_some() && route == Some(Route::Servers)) {
            return Err(Refusal::Forbidden);
        }
        if !self.valid_origin(request, &lease) {
            return Err(Refusal::Forbidden);
        }
        Ok(Authorization::Authenticated(lease))
    }

    fn authenticate<B>(&self, request: &Request<B>) -> Option<AuthLease> {
        if route::lookup(request.uri().path()).is_some_and(|route| route.kind() == Kind::WebSocket)
            && let Some(token) = query(request, "token")
        {
            return self.consume_ticket(request, &token);
        }
        if request.headers().contains_key(header::AUTHORIZATION) {
            return self.bearer(request.headers());
        }
        self.sessions
            .lookup(cookie(request.headers(), "__Host-gm_session")?)
            .map(AuthLease::cookie)
    }

    fn bearer(&self, headers: &HeaderMap) -> Option<AuthLease> {
        self.sessions
            .lookup_bearer(single_header(headers, header::AUTHORIZATION.as_str())?.strip_prefix("Bearer ")?)
    }

    fn consume_ticket<B>(&self, request: &Request<B>, token: &str) -> Option<AuthLease> {
        // Invalid binding is passed through, not rejected before redemption:
        // even a failed redemption must consume this one-shot credential.
        let target = authority(request)
            .and_then(|host| canonical_origin(&format!("https://{host}")).ok())
            .map(|origin| origin + request.uri().path())
            .unwrap_or_default();
        let origin = text(request.headers(), header::ORIGIN.as_str()).unwrap_or("\0");
        self.sessions.consume_ticket(token, &target, origin)
    }

    fn valid_origin<B>(&self, request: &Request<B>, lease: &AuthLease) -> bool {
        let Some(origin) = text(request.headers(), header::ORIGIN.as_str()) else {
            return false;
        };
        let route = route::lookup(request.uri().path());
        if let Some(approved) = lease.browser_origin() {
            return origin == approved && route.is_some_and(|route| route != Route::Servers);
        }
        if !origin.is_empty() && origin != self.public {
            return false;
        }
        if lease.is_bearer() {
            return true;
        }
        let Some(site) = text(request.headers(), "sec-fetch-site") else {
            return false;
        };
        if !matches!(site, "" | "same-origin" | "same-site" | "none") || site == "same-site" && origin != self.public {
            return false;
        }
        let safe = matches!(*request.method(), Method::GET | Method::HEAD);
        if route.is_some() && safe && origin != self.public && site != "same-origin" {
            return false;
        }
        if route == Some(Route::Ping) && origin != self.public {
            return false;
        }
        if safe || request.method() == Method::OPTIONS {
            return true;
        }
        origin == self.public
            && (route.is_none()
                || text(request.headers(), "x-csrf-token")
                    .is_some_and(|csrf| constant_equal(lease.session().csrf(), csrf)))
    }

    fn browser_preflight(&self, headers: &HeaderMap) -> Result<HeaderMap, Refusal> {
        let origin = unique_header(headers, header::ORIGIN.as_str()).ok_or(Refusal::Forbidden)?;
        if !origin.to_str().is_ok_and(super::secure_browser_origin)
            || text(headers, header::ACCESS_CONTROL_REQUEST_METHOD.as_str()) != Some("POST")
            || !text(headers, header::ACCESS_CONTROL_REQUEST_HEADERS.as_str()).is_some_and(|value| {
                value
                    .split(',')
                    .all(|name| name.trim().is_empty() || name.trim().eq_ignore_ascii_case("content-type"))
            })
        {
            return Err(Refusal::Forbidden);
        }
        let mut response = HeaderMap::new();
        Access::Bearer(origin).apply_response(&mut response);
        response.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("POST"));
        response.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Content-Type"),
        );
        response.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("7200"));
        Ok(response)
    }

    fn trusted_peer(&self, address: IpAddr) -> bool {
        self.trusted
            .iter()
            .any(|prefix| prefix.contains(&address.to_canonical()))
    }
}

pub fn constant_equal(expected: &str, actual: &str) -> bool {
    expected.len() > 20 && expected.as_bytes().ct_eq(actual.as_bytes()).into()
}

/// Go's `CookiesNamed` for one cookie: invalid pairs are skipped, and two valid ones are ambiguous.
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let values = headers.get_all(header::COOKIE);
    let count: usize = values
        .iter()
        .map(|value| value.as_bytes().iter().filter(|byte| **byte == b';').count() + 1)
        .sum();
    if count > 3000 {
        return None;
    }
    let mut found = None;
    for part in values
        .iter()
        .flat_map(|header| header.as_bytes().split(|byte| *byte == b';'))
    {
        let part = part.trim_ascii();
        let (key, value) = match part.iter().position(|byte| *byte == b'=') {
            Some(at) => (&part[..at], &part[at + 1..]),
            None => (part, &b""[..]),
        };
        let value = match value {
            [b'"', value @ .., b'"'] => value,
            value => value,
        };
        let valid = value
            .iter()
            .all(|byte| (0x20..0x7f).contains(byte) && !matches!(byte, b'"' | b';' | b'\\'));
        if key.trim_ascii() == name.as_bytes() && valid && found.replace(value).is_some() {
            return None;
        }
    }
    found.map(|value| std::str::from_utf8(value).expect("cookie values are ASCII"))
}

fn authority<B>(request: &Request<B>) -> Option<&str> {
    let host = if request.headers().contains_key(header::HOST) {
        Some(unique_header(request.headers(), header::HOST.as_str())?.to_str().ok()?)
    } else {
        None
    };
    match (request.uri().authority(), host) {
        (Some(authority), Some(host)) if !authority.as_str().eq_ignore_ascii_case(host) => None,
        (Some(authority), _) => Some(authority.as_str()),
        (None, host) => host,
    }
}

fn text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    match unique_header(headers, name) {
        Some(value) => value.to_str().ok(),
        None if headers.contains_key(name) => None,
        None => Some(""),
    }
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let value = unique_header(headers, name)?.to_str().ok()?;
    (!value.contains(',')).then(|| value.trim())
}

fn equal_host(first: &str, second: &str) -> bool {
    first
        .strip_suffix('.')
        .unwrap_or(first)
        .eq_ignore_ascii_case(second.strip_suffix('.').unwrap_or(second))
}

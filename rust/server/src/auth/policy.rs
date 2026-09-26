//! Request authorization decisions, separate from response bodies and transport IO.

use super::{AuthLease, SessionStore};
use crate::{
    config::{AuthMode, ConfigError},
    cors::{self, Access},
};
use graphite_meter_core::{
    origin::{canonical_origin, target_origin},
    route::{self, Kind, Route},
};
use http::{HeaderMap, HeaderValue, Method, Request, header};
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
    route: Option<Route>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionEnded;

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
    pub fn measurement_route(&self) -> Option<Route> {
        self.route
    }
    pub fn map_body<C>(self, map: impl FnOnce(B) -> C) -> AuthorizedRequest<C> {
        AuthorizedRequest {
            request: self.request.map(map),
            authorization: self.authorization,
            connection: self.connection,
            route: self.route,
        }
    }

    /// Collect or adapt a body without changing the authority or route checked
    /// by the policy. The caller supplies the size and time bounds.
    pub async fn try_map_body<C, E>(
        self,
        map: impl AsyncFnOnce(B) -> Result<C, E>,
    ) -> Result<AuthorizedRequest<C>, E> {
        let (parts, body) = self.request.into_parts();
        Ok(AuthorizedRequest {
            request: Request::from_parts(parts, map(body).await?),
            authorization: self.authorization,
            connection: self.connection,
            route: self.route,
        })
    }

    /// Only the transport dispatcher may split the checked request from its
    /// lease. It must retain that lease through the complete IO lifetime.
    pub(crate) fn into_parts(self) -> (Request<B>, Authorization, Connection, Option<Route>) {
        (
            self.request,
            self.authorization,
            self.connection,
            self.route,
        )
    }

    /// Observe revocation before polling the operation, including a lease that
    /// ended between authorization and activation. The operation must own its
    /// complete transport lifetime, not merely prepare a streaming response.
    pub async fn run<F, T>(self, operation: F) -> Result<T, SessionEnded>
    where
        F: AsyncFnOnce(Self) -> T,
    {
        let Authorization::Authenticated(lease) = &self.authorization else {
            return Ok(operation(self).await);
        };
        let lease = lease.clone();
        tokio::select! {
            biased;
            _ = lease.ended() => Err(SessionEnded),
            result = operation(self) => Ok(result),
        }
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
    pub fn new(
        public: &str,
        mode: AuthMode,
        trusted: Vec<IpNet>,
        sessions: SessionStore,
    ) -> Result<Self, ConfigError> {
        let origin = target_origin(public)?.ok_or("authentication requires a public origin")?;
        if mode == AuthMode::Off || origin.scheme != "https" || canonical_origin(public)? != public
        {
            return Err(
                "authentication requires an enabled mode and canonical HTTPS origin".into(),
            );
        }
        Ok(Self {
            public: public.into(),
            public_header: HeaderValue::from_str(public)?,
            hostname: origin.host,
            authority: public
                .strip_prefix("https://")
                .expect("validated scheme")
                .into(),
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

    /// Login throttling deliberately accepts less proxy metadata than public
    /// probe reporting: a trusted peer must supply exactly one X-Real-IP.
    pub fn client_address(&self, headers: &HeaderMap, peer: SocketAddr) -> Option<IpAddr> {
        let peer = peer.ip().to_canonical();
        if !self.trusted_peer(peer) {
            return Some(peer);
        }
        for name in ["forwarded", "x-forwarded-for"] {
            if headers.get(name).is_some_and(|value| !value.is_empty()) {
                return None;
            }
        }
        single_header(headers, "x-real-ip")?
            .parse::<IpAddr>()
            .ok()
            .map(|address| address.to_canonical())
    }

    pub fn authorize<B>(
        &self,
        request: Request<B>,
        connection: Connection,
    ) -> Result<AuthorizedRequest<B>, Box<RejectedRequest<B>>> {
        match self.evaluate(
            &request,
            connection.peer,
            connection.tls,
            connection.listener,
        ) {
            Ok(authorization) => Ok(AuthorizedRequest {
                route: route::lookup(request.uri().path()),
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
        let trust = self.trust(request, peer, tls);
        if tls && !trust.secure {
            return Err(Refusal::AuthenticationRequired);
        }
        let path = request.uri().path();
        let route = route::lookup(path);
        if request.method() == Method::OPTIONS {
            if trust.secure && trust.canonical && path == "/auth/browser/token" {
                return self
                    .browser_preflight(request.headers())
                    .map(Authorization::Preflight);
            }
            if route.is_some() {
                let access = cors::authenticated_preflight(
                    &self.public_header,
                    trust.secure,
                    route,
                    request.headers(),
                )
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
            let redeemed = ticket
                .as_deref()
                .and_then(|token| self.consume_ticket(request, token));
            let lease = bearer.or(redeemed).ok_or(Refusal::AuthenticationRequired)?;
            if !self.valid_origin(request, &lease) {
                return Err(Refusal::AuthenticationRequired);
            }
            return Ok(Authorization::Authenticated(lease));
        }
        if (path == "/login" || path.starts_with("/auth/")) && (!listener.ui || !trust.canonical) {
            return Err(Refusal::Forbidden);
        }
        if listener.ui && self.public_auth_route(request.method(), path) {
            return if trust.secure && trust.canonical {
                Ok(Authorization::PublicAuth)
            } else {
                Err(Refusal::AuthenticationRequired)
            };
        }
        if !trust.secure {
            return Err(Refusal::AuthenticationRequired);
        }
        let lease = self
            .authenticate(request)
            .ok_or(Refusal::AuthenticationRequired)?;
        if lease.is_bearer()
            && (route.is_none()
                || lease.browser_origin().is_some() && route == Some(Route::Servers))
        {
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
        self.sessions.lookup_bearer(
            single_header(headers, header::AUTHORIZATION.as_str())?.strip_prefix("Bearer ")?,
        )
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
        if !matches!(site, "" | "same-origin" | "same-site" | "none")
            || site == "same-site" && origin != self.public
        {
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
        let raw = origin.to_str().map_err(|_| Refusal::Forbidden)?;
        if !raw.starts_with("https://")
            || canonical_origin(raw).ok().as_deref() != Some(raw)
            || text(headers, header::ACCESS_CONTROL_REQUEST_METHOD.as_str()) != Some("POST")
            || !text(headers, header::ACCESS_CONTROL_REQUEST_HEADERS.as_str()).is_some_and(
                |value| {
                    value.split(',').all(|name| {
                        name.trim().is_empty() || name.trim().eq_ignore_ascii_case("content-type")
                    })
                },
            )
        {
            return Err(Refusal::Forbidden);
        }
        let mut response = HeaderMap::new();
        Access::Bearer(origin).apply_response(&mut response);
        response.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("POST"),
        );
        response.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Content-Type"),
        );
        Ok(response)
    }

    fn public_auth_route(&self, method: &Method, path: &str) -> bool {
        method == Method::GET && matches!(path, "/login" | "/auth/cli" | "/auth/browser")
            || method == Method::POST && matches!(path, "/auth/cli/token" | "/auth/browser/token")
            || self.mode.password() && method == Method::POST && path == "/auth/password"
            || self.mode.oidc()
                && (method == Method::POST && path == "/auth/oidc/start"
                    || method == Method::GET && path == "/auth/oidc/callback")
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

pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let values = headers.get_all(header::COOKIE);
    let count: usize = values
        .iter()
        .map(|value| {
            value
                .as_bytes()
                .iter()
                .filter(|byte| **byte == b';')
                .count()
                + 1
        })
        .sum();
    if count > 3000 {
        return None;
    }
    let mut found = None;
    for header in values {
        let Ok(header) = header.to_str() else {
            return None;
        };
        for part in header.split(';').map(str::trim) {
            let Some((key, value)) = part.split_once('=') else {
                continue;
            };
            if key.trim() != name {
                continue;
            }
            if found.is_some() {
                return None;
            }
            let value = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(value);
            if !value
                .bytes()
                .all(|byte| (0x20..0x7f).contains(&byte) && !matches!(byte, b'"' | b';' | b'\\'))
            {
                return None;
            }
            found = Some(value);
        }
    }
    found
}

fn query<B>(request: &Request<B>, name: &str) -> Option<String> {
    form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

fn authority<B>(request: &Request<B>) -> Option<&str> {
    let host = if request.headers().contains_key(header::HOST) {
        Some(
            unique_header(request.headers(), header::HOST.as_str())?
                .to_str()
                .ok()?,
        )
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

fn unique_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a HeaderValue> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    values.next().is_none().then_some(value)
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

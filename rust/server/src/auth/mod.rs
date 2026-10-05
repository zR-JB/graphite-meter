//! Authentication: the policy every request passes, its own routes, and the lease a signed-in request carries.

mod approval;
mod jwt;
mod oidc;
mod page;
pub mod password;
mod policy;
mod provider;
mod rate;
mod routes;
mod security;
mod store;

pub use policy::Policy;
pub use security::{COUNTERS, Security};
pub use store::{GrantRefusal, LOGIN_LIFETIME, LoginKey, NewLogin, Store};

use crate::{
    app::{Endpoint, Outcome, finalize::Access, response},
    config::{self, Methods},
    log,
    peer::{ClientKeys, Peer},
    transport::body::Body,
};
use graphite_meter_proto::{
    duration,
    route::{Kind, Route},
    token::SocketTicket,
};
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use oidc::Oidc;
use page::protect;
use password::Password;
use rate::Attempts;
use std::{future::Future, sync::Arc};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

/// What the policy decided for a request.
#[derive(Debug)]
pub enum Decision {
    /// Serve it, as the lease's holder when signed in.
    Allow(Option<AuthLease>),
    /// The policy's own answer from the head: a refusal, or an authenticated preflight.
    Answer(Response<Body>),
}

/// The configured authentication.
pub enum Auth {
    /// Every request is allowed, anonymously.
    Off,
    /// Sign-in at the public origin.
    On(Box<Enabled>),
}

/// Sign-in at the public origin: the policy, the store of logins, grants and tickets, password and OIDC sign-in, the
/// approval budget and the security log.
pub struct Enabled {
    policy: Policy,
    store: Store,
    password: Option<Password>,
    oidc: Option<Oidc>,
    /// Approval pages opened per client address.
    browser_approvals: Attempts,
    security: Security,
    /// The OIDC provider's name, which the sign-in page shows.
    provider: String,
}

impl Auth {
    /// The authentication `config` enables, with its password hash and OIDC client secret read; the OIDC provider is
    /// discovered later.
    pub fn new(config: Option<&config::Auth>, verbose: bool) -> Result<Self, String> {
        let Some(config) = config else { return Ok(Self::Off) };
        let (mode, settings) = match &config.methods {
            Methods::Password(_) => ("password", None),
            Methods::Oidc(oidc) => ("oidc", Some(oidc)),
            Methods::Hybrid(_, oidc) => ("hybrid", Some(oidc)),
        };
        let security = Security::new(verbose);
        let password = match config.methods.password() {
            Some(secret) => {
                let encoded = secret.read(4096).map_err(|error| format!("password hash: {error}"))?;
                let password = Password::new(&encoded)?;
                security.debug(format_args!("local password hash loaded and validated"));
                Some(password)
            }
            None => None,
        };
        let oidc = match settings {
            Some(settings) => {
                let secret = settings.secret.read(16 * 1024);
                let secret = secret.map_err(|error| format!("OIDC client secret: {error}"))?;
                Some(Oidc::new(&config.public_origin, settings, secret)?)
            }
            None => None,
        };
        log!(
            "[gm:auth] mode={mode} origin={} provider={} issuer={} allowed-groups={} session-lifetime={}",
            config.public_origin,
            config.provider,
            settings.map_or("", |oidc| oidc.issuer.as_str()),
            settings.map_or(0, |oidc| oidc.allowed_groups.len()),
            duration::format(LOGIN_LIFETIME),
        );
        Ok(Self::On(Box::new(Enabled {
            policy: Policy::new(config),
            store: Store::default(),
            password,
            oidc,
            browser_approvals: Attempts::new("browser-approval", 10),
            security,
            provider: config.provider.clone(),
        })))
    }

    /// Discovers the OIDC provider once, when one is configured; OIDC mode serves only after this succeeds.
    pub async fn discover(&self) -> Result<(), String> {
        let Self::On(auth) = self else { return Ok(()) };
        match &auth.oidc {
            Some(oidc) => oidc.discover(&auth.security).await,
            None => Ok(()),
        }
    }

    /// Hybrid mode's discovery, retried in the background until the provider answers; it never completes.
    pub fn background_discovery(&self) -> Option<impl Future<Output = ()> + Send + '_> {
        let Self::On(auth) = self else { return None };
        let oidc = auth.oidc.as_ref().filter(|_| auth.password.is_some())?;
        Some(oidc.retry(&auth.security))
    }

    pub fn enabled(&self) -> bool {
        matches!(self, Self::On(_))
    }

    /// The store of logins, grants and tickets, when enabled.
    pub fn store(&self) -> Option<&Store> {
        match self {
            Self::Off => None,
            Self::On(auth) => Some(&auth.store),
        }
    }

    /// The security log's counts, when enabled.
    pub fn security(&self) -> Option<&Security> {
        match self {
            Self::Off => None,
            Self::On(auth) => Some(&auth.security),
        }
    }

    /// The policy every request passes, decided from its head alone.
    pub fn authorize<B>(&self, request: &Request<B>, endpoint: Endpoint, peer: &Peer) -> Decision {
        match self {
            Self::Off => Decision::Allow(None),
            Self::On(auth) => auth.policy.authorize(&auth.store, request, endpoint, peer),
        }
    }

    /// Whether `path` is one of the routes `handle` answers on the endpoints serving the app; none when off.
    pub fn claims(&self, path: &str) -> bool {
        self.enabled() && (path == "/login" || path.starts_with("/auth/"))
    }

    /// Answers an authentication route by its exchange's `deadline`, reading its body: pages, sign-in posts, the OIDC
    /// callback, approvals and logout.
    pub async fn handle<B: http_body::Body>(&self, request: Request<B>, deadline: Instant, peer: &Peer) -> Outcome {
        let response = match self {
            Self::Off => response::status(StatusCode::NOT_FOUND),
            Self::On(auth) => routes::handle(auth, request, deadline, peer).await,
        };
        Outcome::Response(response)
    }

    /// Who may read the answer to a request for `route` from another origin.
    pub(crate) fn access(
        &self,
        route: Option<Route>,
        lease: Option<&AuthLease>,
        headers: &HeaderMap,
    ) -> Option<Access> {
        route?;
        match self {
            Self::Off => Some(Access::Public),
            Self::On(auth) => auth.policy.access(lease?, headers),
        }
    }

    /// The answer to a socket-ticket mint for routes of `kind`.
    pub(crate) fn ticket<B>(&self, request: &Request<B>, lease: Option<&AuthLease>, kind: Kind) -> Response<Body> {
        match self {
            Self::Off => response::json_of(&SocketTicket::unauthenticated()),
            Self::On(auth) => routes::mint(auth, request, lease, kind),
        }
    }
}

/// What a lease is held through: a login's session or a measurement grant issued to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Holder {
    Login(Arc<str>),
    Grant(Arc<str>),
}

/// How a lease's credential was presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// The login's session cookie.
    Cookie,
    /// A grant's bearer token or a socket ticket, with the browser origin a browser grant serves.
    Bearer(Option<HeaderValue>),
}

/// A signed-in request's identity, and what ends it: its login's expiry, sign-out, or the grant's revocation.
#[derive(Debug, Clone)]
pub struct AuthLease {
    holder: Holder,
    principal: Arc<str>,
    login: LoginKey,
    via: Via,
    revoked: CancellationToken,
    expires: Instant,
}

impl AuthLease {
    pub fn holder(&self) -> &Holder {
        &self.holder
    }

    pub fn via(&self) -> &Via {
        &self.via
    }

    /// The browser origin a browser grant serves.
    pub fn browser(&self) -> Option<&HeaderValue> {
        match &self.via {
            Via::Bearer(browser) => browser.as_ref(),
            Via::Cookie => None,
        }
    }

    /// The login it belongs to.
    pub fn login(&self) -> LoginKey {
        self.login
    }

    /// The holder, then its principal at twice the share: admission keys and the owner of uploads it creates.
    pub fn keys(&self) -> ClientKeys {
        ClientKeys::Auth(self.holder.clone(), self.principal.clone())
    }

    /// Whether it was revoked, or expired by `now`.
    pub fn is_ended(&self, now: Instant) -> bool {
        self.revoked.is_cancelled() || now >= self.expires
    }

    /// Completes once it is revoked or expires; lanes end with `revoked` then.
    pub async fn ended(&self) {
        tokio::select! {
            () = self.revoked.cancelled() => {}
            () = sleep_until(self.expires) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::Address;

    #[test]
    fn with_authentication_off_every_request_is_anonymous() {
        let request = Request::new(());
        let peer = Peer::new(Address::Socket("192.0.2.1".parse().unwrap()));
        let decision = Auth::Off.authorize(&request, Endpoint::H1, &peer);
        assert!(matches!(decision, Decision::Allow(None)));
        assert!(!Auth::Off.claims("/login") && !Auth::Off.claims("/auth/password"));
    }
}

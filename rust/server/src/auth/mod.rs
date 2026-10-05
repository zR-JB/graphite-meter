//! Authentication: the policy every request passes, its own routes, and the lease a signed-in request carries.

pub mod password;
mod policy;
mod routes;
mod store;

pub use policy::Policy;
pub use store::{GrantRefusal, LOGIN_LIFETIME, LoginKey, NewLogin, Store};

use crate::{
    app::{Endpoint, Outcome, finalize::Access, response},
    config,
    peer::{ClientKeys, Peer},
    transport::body::Body,
};
use graphite_meter_proto::{
    route::{Kind, Route},
    token::SocketTicket,
};
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header};
use std::sync::Arc;
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
    /// Sign-in at the public origin, and the store of its logins, grants and tickets.
    On { policy: Policy, store: Store },
}

impl Auth {
    /// The authentication `config` enables; OIDC sign-in is refused as unavailable.
    pub fn new(config: Option<&config::Auth>) -> Result<Self, String> {
        let Some(config) = config else { return Ok(Self::Off) };
        if config.methods.oidc().is_some() {
            return Err("OIDC sign-in is unavailable in this build".into());
        }
        Ok(Self::On { policy: Policy::new(config), store: Store::default() })
    }

    pub fn enabled(&self) -> bool {
        matches!(self, Self::On { .. })
    }

    /// The store of logins, grants and tickets, when enabled.
    pub fn store(&self) -> Option<&Store> {
        match self {
            Self::Off => None,
            Self::On { store, .. } => Some(store),
        }
    }

    /// The policy every request passes, decided from its head alone.
    pub fn authorize<B>(&self, request: &Request<B>, endpoint: Endpoint, peer: &Peer) -> Decision {
        match self {
            Self::Off => Decision::Allow(None),
            Self::On { policy, store } => policy.authorize(store, request, endpoint, peer),
        }
    }

    /// Whether `path` is one of the routes `handle` answers on the endpoints serving the app; none when off.
    pub fn claims(&self, path: &str) -> bool {
        self.enabled() && (path == "/login" || path.starts_with("/auth/"))
    }

    /// Answers an authentication route, reading its body: pages, sign-in posts, the OIDC callback, approvals and
    /// logout.
    pub async fn handle<B: http_body::Body>(&self, request: Request<B>, _endpoint: Endpoint, peer: &Peer) -> Outcome {
        let response = match self {
            Self::Off => response::status(StatusCode::NOT_FOUND),
            Self::On { policy, store } => routes::handle(policy, store, request, peer.auth()).await,
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
            Self::On { policy, .. } => policy.access(lease?, headers),
        }
    }

    /// The answer to a socket-ticket mint for routes of `kind`.
    pub(crate) fn ticket<B>(&self, request: &Request<B>, lease: Option<&AuthLease>, kind: Kind) -> Response<Body> {
        match self {
            Self::Off => response::json_of(&SocketTicket::unauthenticated()),
            Self::On { policy, store } => routes::mint(policy, store, request, lease, kind),
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

/// Go's headers of every authentication page and refusal, with HSTS once the request is known to be secure.
fn protect(headers: &mut HeaderMap, secure: bool) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};
    use std::sync::LazyLock;
    static POLICY: LazyLock<HeaderValue> = LazyLock::new(|| {
        let hash = |asset: &str| STANDARD.encode(Sha256::digest(asset));
        let styles = hash(include_str!("../../../../go/internal/auth/assets/auth.css"));
        let theme = hash(include_str!("../../../../go/internal/auth/assets/theme.js"));
        let pending = hash(include_str!("../../../../go/internal/auth/assets/pending.js"));
        let policy = format!(
            "default-src 'none'; style-src 'sha256-{styles}'; font-src 'self'; script-src 'sha256-{theme}' \
             'sha256-{pending}'; connect-src 'self'; img-src data:; form-action 'self'; frame-ancestors 'none'; \
             base-uri 'none'"
        );
        HeaderValue::from_str(&policy).expect("hashes are base64")
    });
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    crate::app::finalize::harden(headers, secure);
    headers.insert(header::CONTENT_SECURITY_POLICY, POLICY.clone());
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

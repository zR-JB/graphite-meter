//! Authentication: the policy every request passes, its own routes, and the lease a signed-in request carries.

pub mod password;

use crate::{
    app::{Endpoint, Outcome, response},
    peer::{ClientKeys, Peer},
    transport::body::Body,
};
use http::{Request, Response, StatusCode};
use std::sync::Arc;
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
#[derive(Debug)]
pub enum Auth {
    /// Every request is allowed, anonymously.
    Off,
}

impl Auth {
    /// The policy every request passes, decided from its head alone.
    pub fn authorize<B>(&self, _request: &Request<B>, _endpoint: Endpoint, _peer: &Peer) -> Decision {
        match self {
            Self::Off => Decision::Allow(None),
        }
    }

    /// Whether `path` is one of the routes `handle` answers on the endpoints serving the app; none when off.
    pub fn claims(&self, _path: &str) -> bool {
        match self {
            Self::Off => false,
        }
    }

    /// Answers an authentication route, reading its body: pages, sign-in posts, the OIDC callback, approvals and
    /// logout.
    pub async fn handle<B: http_body::Body>(&self, _request: Request<B>, _endpoint: Endpoint, _peer: &Peer) -> Outcome {
        match self {
            Self::Off => Outcome::Response(response::status(StatusCode::NOT_FOUND)),
        }
    }
}

/// What a lease is held through: a login's session or a measurement grant issued to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Holder {
    Login(Arc<str>),
    Grant(Arc<str>),
}

/// A signed-in request's identity, and the token revoking it when its login or grant ends.
#[derive(Debug, Clone)]
pub struct AuthLease {
    holder: Holder,
    principal: Arc<str>,
    revoked: CancellationToken,
}

impl AuthLease {
    pub fn new(holder: Holder, principal: impl Into<Arc<str>>, revoked: CancellationToken) -> Self {
        Self { holder, principal: principal.into(), revoked }
    }

    pub fn holder(&self) -> &Holder {
        &self.holder
    }

    /// The subject every login and grant of one user shares.
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// The holder, then its principal at twice the share: admission keys and the owner of uploads it creates.
    pub fn keys(&self) -> ClientKeys {
        ClientKeys::Auth(self.holder.clone(), self.principal.clone())
    }

    /// Cancelled on sign-out, renewal or expiry; lanes end with `revoked` then.
    pub fn revoked(&self) -> &CancellationToken {
        &self.revoked
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

//! Authenticates requests with the policy each one passes, its own routes and the lease a signed-in request carries.

mod approval;
mod jwt;
mod oidc;
mod page;
pub mod password;
mod policy;
mod provider;
mod rate;
mod routes;
mod store;

pub use policy::{BrowserOrigin, Policy};
pub use store::{
    CsrfToken, GrantRefusal, GrantToken, Identity, LOGIN_LIFETIME, LoginKey, NewLogin, SessionToken, Store,
};

use crate::{
    app::{Endpoint, Outcome, finalize::Access, response},
    config::{self, Methods},
    log,
    peer::{ClientKeys, Peer},
    transport::body::Body,
};
use graphite_meter_proto::{
    route::{Kind, Route},
    token::SocketTicket,
};
use http::{HeaderMap, Request, Response, StatusCode};
use oidc::Oidc;
use page::protect;
use password::Password;
use rate::Attempts;
use std::{
    fmt::{self, Write as _},
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

/// The policy decided to serve the request, as the lease's holder when signed in, or to answer from the head itself.
pub type Decision = Result<Option<AuthLease>, Response<Body>>;

/// Holds the configured authentication. Without it every request is allowed, anonymously.
pub struct Auth(Option<Box<Enabled>>);

/// Signs in at the public origin with policy, login store, password and OIDC sign-in, approval budget and security log.
pub struct Enabled {
    policy: Policy,
    store: Store,
    sign_in: SignIn,
    /// Counts approval pages opened per client address.
    browser_approvals: Attempts,
    security: Security,
    /// The sign-in page shows this OIDC provider name.
    provider: String,
}

impl Auth {
    /// Builds what `config` enables, reading its hash and OIDC secret. The OIDC provider is discovered later.
    pub fn new(config: Option<&config::Auth>, outbound: &config::Outbound, verbose: bool) -> Result<Self, String> {
        let Some(config) = config else { return Ok(Self(None)) };
        let (mode, settings) = match &config.methods {
            Methods::Password(_) => ("password", None),
            Methods::Oidc(oidc) => ("OIDC", Some(oidc)),
            Methods::Hybrid(_, oidc) => ("password and OIDC", Some(oidc)),
        };
        let security = Security::new(verbose);
        let load = |secret: &config::Secret| {
            let encoded = secret.read(4096).map_err(|error| format!("password hash: {error}"))?;
            let verifier = Password::new(&encoded)?;
            security.debug(format_args!("password hash loaded: valid"));
            Ok::<_, String>(verifier)
        };
        let oidc = |settings: &config::Oidc| {
            let secret = settings.secret.read(16 * 1024);
            let secret = secret.map_err(|error| format!("OIDC client secret: {error}"))?;
            Oidc::new(&config.public_origin, settings, secret, outbound)
        };
        let sign_in = match &config.methods {
            Methods::Password(secret) => SignIn::Password(load(secret)?),
            Methods::Oidc(settings) => SignIn::Oidc(Box::new(oidc(settings)?)),
            Methods::Hybrid(secret, settings) => SignIn::Hybrid(load(secret)?, Box::new(oidc(settings)?)),
        };
        let lifetime = log::span(LOGIN_LIFETIME);
        log!(Info, "auth", "{mode} sign-in at {}; sessions last {lifetime}", config.public_origin);
        if let Some(oidc) = settings {
            let groups = match oidc.allowed_groups.as_slice() {
                [_] => "1 group allowed".to_owned(),
                groups => format!("{} groups allowed", groups.len()),
            };
            log!(Info, "auth", "OIDC provider {} at {}; {groups}", config.provider, oidc.issuer);
        }
        Ok(Self(Some(Box::new(Enabled {
            policy: Policy::new(config),
            store: Store::default(),
            sign_in,
            browser_approvals: Attempts::new("browser-approval", 10),
            security,
            provider: config.provider.clone(),
        }))))
    }

    /// Discovers the OIDC provider once, when one is configured. OIDC mode serves only after this succeeds.
    pub async fn discover(&self) -> Result<(), String> {
        match self.0.as_ref().and_then(|auth| auth.sign_in.oidc()) {
            Some(oidc) => oidc.discover().await,
            None => Ok(()),
        }
    }

    /// Retries hybrid mode's discovery in the background until the provider answers.
    pub fn background_discovery(&self) -> Option<impl Future<Output = ()> + Send + '_> {
        let Some(auth) = &self.0 else { return None };
        let SignIn::Hybrid(_, oidc) = &auth.sign_in else { return None };
        Some(oidc.retry())
    }

    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Gives the store of logins, grants and tickets, when enabled.
    pub fn store(&self) -> Option<&Store> {
        self.0.as_ref().map(|auth| &auth.store)
    }

    /// Gives the security log's counts, when enabled.
    pub fn security(&self) -> Option<&Security> {
        self.0.as_ref().map(|auth| &auth.security)
    }

    /// Applies the policy every request passes, decided from its head alone.
    pub fn authorize<B>(&self, request: &Request<B>, endpoint: Endpoint, peer: &Peer) -> Decision {
        match &self.0 {
            None => Ok(None),
            Some(auth) => auth.policy.authorize(&auth.store, request, endpoint, peer),
        }
    }

    /// Says whether `path` is one of the routes `handle` answers on the endpoints serving the app. None are when off.
    pub fn claims(&self, path: &str) -> bool {
        self.enabled() && (path == "/login" || path.starts_with("/auth/"))
    }

    /// Answers an authentication route by `deadline`, a page, sign-in post, the OIDC callback, an approval or logout.
    pub async fn handle<B: http_body::Body>(&self, request: Request<B>, deadline: Instant, peer: &Peer) -> Outcome {
        let response = match &self.0 {
            None => response::status(StatusCode::NOT_FOUND),
            Some(auth) => routes::handle(auth, request, deadline, peer).await,
        };
        Outcome::Response(response)
    }

    /// Says who may read the answer to a request for `route` from another origin.
    pub(crate) fn access(
        &self,
        route: Option<Route>,
        lease: Option<&AuthLease>,
        headers: &HeaderMap,
    ) -> Option<Access> {
        route?;
        match &self.0 {
            None => Some(Access::Public),
            Some(auth) => auth.policy.access(lease?, headers),
        }
    }

    /// Answers a socket-ticket mint for routes of `kind`.
    pub(crate) fn ticket<B>(&self, request: &Request<B>, lease: Option<&AuthLease>, kind: Kind) -> Response<Body> {
        match &self.0 {
            None => response::json_of(&SocketTicket::unauthenticated()),
            Some(auth) => routes::mint(auth, request, lease, kind),
        }
    }
}

/// People sign in, as configured, with the password, OIDC or both.
enum SignIn {
    Password(Password),
    Oidc(Box<Oidc>),
    Hybrid(Password, Box<Oidc>),
}

impl SignIn {
    fn password(&self) -> Option<&Password> {
        match self {
            Self::Password(password) | Self::Hybrid(password, _) => Some(password),
            Self::Oidc(_) => None,
        }
    }

    fn oidc(&self) -> Option<&Oidc> {
        match self {
            Self::Oidc(oidc) | Self::Hybrid(_, oidc) => Some(oidc),
            Self::Password(_) => None,
        }
    }
}

/// Names who a login belongs to, the key its admission share and uploads count under.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Principal(Arc<str>);

impl Principal {
    /// Every password login shares this principal.
    pub fn operator() -> Self {
        Self("local-operator".into())
    }

    /// Names the principal of an OIDC subject.
    pub fn oidc(subject: &str) -> Self {
        Self(format!("oidc:{subject}").into())
    }
}

/// A lease is held through a login's session or a measurement grant issued to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Holder {
    Login(Arc<str>),
    Grant(Arc<str>),
}

/// Says how a lease's credential was presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// The login's session cookie carried it.
    Cookie,
    /// A grant's bearer token or a socket ticket carried it, with the browser origin a browser grant serves.
    Bearer(Option<BrowserOrigin>),
}

/// Carries a signed-in request's identity and what ends it, its login's expiry, sign-out or the grant's revocation.
#[derive(Debug, Clone)]
pub struct AuthLease {
    holder: Holder,
    principal: Principal,
    login: LoginKey,
    via: Via,
    revoked: CancellationToken,
    expires: Instant,
}

impl AuthLease {
    pub fn via(&self) -> &Via {
        &self.via
    }

    /// Gives the browser origin a browser grant serves.
    pub fn browser(&self) -> Option<&BrowserOrigin> {
        match &self.via {
            Via::Bearer(browser) => browser.as_ref(),
            Via::Cookie => None,
        }
    }

    /// Gives the login it belongs to.
    pub fn login(&self) -> LoginKey {
        self.login
    }

    /// Gives the holder, then its principal at twice the share, as admission keys and the owner of uploads it creates.
    pub fn keys(&self) -> ClientKeys {
        ClientKeys::Auth(self.holder.clone(), self.principal.clone())
    }

    /// Says whether it was revoked, or expired by `now`.
    pub fn is_ended(&self, now: Instant) -> bool {
        self.revoked.is_cancelled() || now >= self.expires
    }

    /// Completes once it is revoked or expires. Lanes end with `revoked` then.
    pub async fn ended(&self) {
        tokio::select! {
            () = self.revoked.cancelled() => {}
            () = sleep_until(self.expires) => {}
        }
    }
}

graphite_meter_proto::table! {
    /// Names a counted outcome at its place in the minute line.
    enum Counter {
        name.0: &'static str,
    } {
        Local => ("local",),
        Oidc => ("oidc",),
        InvalidPassword => ("invalid-password",),
        OidcFailure => ("oidc-failure",),
        GroupDenial => ("group-denial",),
        ReplayExpiry => ("replay-expiry",),
        Throttled => ("throttled",),
        Logout => ("logout",),
        CliApproval => ("cli-approval",),
        Capacity => ("capacity",),
    }
}

/// The minute line reports this many counters.
pub const COUNTERS: usize = Counter::ALL.len();

graphite_meter_proto::table! {
    /// Tells why a sign-in was refused, its security log code, the sign-in page's notice and the counter it adds to.
    enum Reason {
        code.0: &'static str,
        notice.1: &'static str,
        counter.2: Option<Counter>,
    } {
        CsrfOriginMissing => ("csrf_origin_missing", "failed", None),
        CsrfOriginMismatch => ("csrf_origin_mismatch", "failed", None),
        CsrfCookieMissing => ("csrf_cookie_missing", "stale", None),
        CsrfTokenMissing => ("csrf_token_missing", "stale", None),
        /// Usually a second sign-in tab replaced the login cookie this form was made for.
        CsrfTokenMismatch => ("csrf_token_mismatch", "stale", None),
        MalformedForm => ("malformed_form", "failed", None),
        Throttled => ("rate_limited", "throttled", Some(Counter::Throttled)),
        ClientAddress => ("client_address", "proxy", None),
        VerifierBusy => ("verifier_busy", "busy", None),
        PasswordMismatch => ("password_mismatch", "password", Some(Counter::InvalidPassword)),
        SessionCapacity => ("session_capacity", "busy", Some(Counter::Capacity)),
        ProviderNotReady => ("provider_not_ready", "provider", None),
        TransactionCapacity => ("transaction_capacity", "busy", Some(Counter::Capacity)),
        ExchangeRateLimited => ("exchange_rate_limited", "failed", None),
        CallbackParameters => ("callback_parameters", "failed", None),
        TransactionCookie => ("transaction_cookie", "stale", None),
        TransactionReplay => ("transaction_replay_or_expiry", "failed", Some(Counter::ReplayExpiry)),
        ResponseIssuer => ("response_issuer", "failed", None),
        TokenExchange => ("token_exchange", "failed", None),
        MissingIdToken => ("missing_id_token", "failed", None),
        IdTokenVerification => ("id_token_verification", "failed", None),
        IdTokenClaimsOrNonce => ("id_token_claims_or_nonce", "failed", None),
        AccessTokenHash => ("access_token_hash", "failed", None),
        UserInfoOrSubject => ("userinfo_or_subject", "failed", None),
        UserInfoClaims => ("userinfo_claims_or_group", "failed", None),
        /// The user has no allowed group, reported as `userinfo_claims_or_group` and counted apart.
        GroupDenied => ("userinfo_claims_or_group", "failed", Some(Counter::GroupDenial)),
        InvalidSubject => ("invalid_subject", "failed", None),
    }
}

/// Counts sign-in outcomes into one security log line a minute and writes debug lines under `GM_VERBOSE`.
pub struct Security {
    counts: [AtomicU64; COUNTERS],
    verbose: bool,
}

impl Security {
    fn new(verbose: bool) -> Self {
        Self { counts: Default::default(), verbose }
    }

    fn count(&self, counter: Counter) {
        self.counts[counter.index()].fetch_add(1, Ordering::Relaxed);
    }

    fn debug(&self, message: fmt::Arguments<'_>) {
        if self.verbose {
            log!(Debug, "auth", "{message}");
        }
    }

    /// Logs a refused sign-in and counts the refusals the minute line reports.
    fn refused(&self, reason: Reason) {
        self.debug(format_args!("sign-in refused: reason={}", reason.code()));
        if let Some(counter) = reason.counter() {
            self.count(counter);
        }
    }

    /// Writes the minute line of the counts since `last`, which it advances, or `None` when nothing changed.
    pub fn line(&self, last: &mut [u64; COUNTERS]) -> Option<String> {
        let counts = self.counts.each_ref().map(|count| count.load(Ordering::Relaxed));
        if counts == *last {
            return None;
        }
        let mut line = String::from("sign-ins in the last minute:");
        for ((counter, count), last) in Counter::ALL.iter().zip(counts).zip(last.iter_mut()) {
            write!(line, " {}={}", counter.name(), count - *last).expect("strings take writes");
            *last = count;
        }
        Some(line)
    }
}

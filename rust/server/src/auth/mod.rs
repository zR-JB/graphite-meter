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
mod store;

pub use policy::Policy;
pub use store::{GrantRefusal, LOGIN_LIFETIME, LoginKey, NewLogin, Store};

/// The principal every password login shares.
pub const OPERATOR: &str = "local-operator";

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

/// What the policy decided: serve it, as the lease's holder when signed in, or its own answer from the head.
pub type Decision = Result<Option<AuthLease>, Response<Body>>;

/// The configured authentication; without it every request is allowed, anonymously.
pub struct Auth(Option<Box<Enabled>>);

/// Sign-in at the public origin: policy, login store, password and OIDC sign-in, approval budget, security log.
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
    /// The authentication `config` enables, its hash and OIDC secret read; the OIDC provider is discovered later.
    pub fn new(config: Option<&config::Auth>, verbose: bool) -> Result<Self, String> {
        let Some(config) = config else { return Ok(Self(None)) };
        let (mode, settings) = match &config.methods {
            Methods::Password(_) => ("password", None),
            Methods::Oidc(oidc) => ("oidc", Some(oidc)),
            Methods::Hybrid(_, oidc) => ("hybrid", Some(oidc)),
        };
        let security = Security::new(verbose);
        let password = |secret: &config::Secret| {
            let encoded = secret.read(4096).map_err(|error| format!("password hash: {error}"))?;
            let password = Password::new(&encoded)?;
            security.debug(format_args!("password hash loaded: valid"));
            Ok::<_, String>(password)
        };
        let password = config.methods.password().map(password).transpose()?;
        let oidc = |settings: &config::Oidc| {
            let secret = settings.secret.read(16 * 1024);
            let secret = secret.map_err(|error| format!("OIDC client secret: {error}"))?;
            Oidc::new(&config.public_origin, settings, secret)
        };
        let oidc = settings.map(oidc).transpose()?;
        let provider = settings.map(|oidc| {
            let groups = oidc.allowed_groups.len();
            format!(" provider={} issuer={} allowed-groups={groups}", config.provider, oidc.issuer)
        });
        let (origin, lifetime) = (&config.public_origin, duration::format(LOGIN_LIFETIME));
        let provider = provider.unwrap_or_default();
        log!(
            Info,
            "auth",
            "sign-in ready: mode={mode} origin={origin}{provider} session-lifetime={lifetime}"
        );
        Ok(Self(Some(Box::new(Enabled {
            policy: Policy::new(config),
            store: Store::default(),
            password,
            oidc,
            browser_approvals: Attempts::new("browser-approval", 10),
            security,
            provider: config.provider.clone(),
        }))))
    }

    /// Discovers the OIDC provider once, when one is configured; OIDC mode serves only after this succeeds.
    pub async fn discover(&self) -> Result<(), String> {
        let Some(auth) = &self.0 else { return Ok(()) };
        match &auth.oidc {
            Some(oidc) => oidc.discover(&auth.security).await,
            None => Ok(()),
        }
    }

    /// Hybrid mode's discovery, retried in the background until the provider answers; it never completes.
    pub fn background_discovery(&self) -> Option<impl Future<Output = ()> + Send + '_> {
        let Some(auth) = &self.0 else { return None };
        let oidc = auth.oidc.as_ref().filter(|_| auth.password.is_some())?;
        Some(oidc.retry(&auth.security))
    }

    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    /// A test hook: the store of logins, grants and tickets, when enabled, for signing in without credentials.
    pub fn store(&self) -> Option<&Store> {
        self.0.as_ref().map(|auth| &auth.store)
    }

    /// The security log's counts, when enabled.
    pub fn security(&self) -> Option<&Security> {
        self.0.as_ref().map(|auth| &auth.security)
    }

    /// The policy every request passes, decided from its head alone.
    pub fn authorize<B>(&self, request: &Request<B>, endpoint: Endpoint, peer: &Peer) -> Decision {
        match &self.0 {
            None => Ok(None),
            Some(auth) => auth.policy.authorize(&auth.store, request, endpoint, peer),
        }
    }

    /// Whether `path` is one of the routes `handle` answers on the endpoints serving the app; none when off.
    pub fn claims(&self, path: &str) -> bool {
        self.enabled() && (path == "/login" || path.starts_with("/auth/"))
    }

    /// Answers an authentication route by `deadline`: pages, sign-in posts, the OIDC callback, approvals, logout.
    pub async fn handle<B: http_body::Body>(&self, request: Request<B>, deadline: Instant, peer: &Peer) -> Outcome {
        let response = match &self.0 {
            None => response::status(StatusCode::NOT_FOUND),
            Some(auth) => routes::handle(auth, request, deadline, peer).await,
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
        match &self.0 {
            None => Some(Access::Public),
            Some(auth) => auth.policy.access(lease?, headers),
        }
    }

    /// The answer to a socket-ticket mint for routes of `kind`.
    pub(crate) fn ticket<B>(&self, request: &Request<B>, lease: Option<&AuthLease>, kind: Kind) -> Response<Body> {
        match &self.0 {
            None => response::json_of(&SocketTicket::unauthenticated()),
            Some(auth) => routes::mint(auth, request, lease, kind),
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

graphite_meter_proto::table! {
    /// A counted outcome, at its place in the minute line.
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

/// How many counters the minute line reports.
pub const COUNTERS: usize = Counter::ALL.len();

graphite_meter_proto::table! {
    /// Why a sign-in was refused: its security log code, the sign-in page's notice and the counter it adds to.
    enum Reason {
        code.0: &'static str,
        notice.1: &'static str,
        counter.2: Option<Counter>,
    } {
        CsrfOriginMissing => ("csrf_origin_missing", "failed", None),
        CsrfOriginMismatch => ("csrf_origin_mismatch", "failed", None),
        CsrfCookieMissing => ("csrf_cookie_missing", "stale", None),
        CsrfTokenMissing => ("csrf_token_missing", "stale", None),
        CsrfTokenMismatch => ("csrf_token_mismatch", "failed", None),
        MalformedForm => ("malformed_form", "failed", None),
        Throttled => ("rate_limited_or_client_address", "throttled", Some(Counter::Throttled)),
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
        /// No allowed group: reported as `userinfo_claims_or_group`, counted apart.
        GroupDenied => ("userinfo_claims_or_group", "failed", Some(Counter::GroupDenial)),
        InvalidSubject => ("invalid_subject", "failed", None),
    }
}

/// The security log: sign-in outcomes counted into one line a minute, and debug lines under `GM_VERBOSE`.
pub struct Security {
    counts: [AtomicU64; COUNTERS],
    verbose: bool,
}

impl Security {
    fn new(verbose: bool) -> Self {
        Self { counts: Default::default(), verbose }
    }

    fn count(&self, counter: Counter) {
        self.counts[counter as usize].fetch_add(1, Ordering::Relaxed);
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

    /// The minute line of the counts since `last`, which it advances; `None` when nothing changed.
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

//! Sign-in approvals: a terminal or browser asks with a challenge, the operator approves, the verifier earns a grant.

use super::{
    AuthLease, BrowserOrigin, Counter, Enabled, LOGIN_LIFETIME, LoginKey, page,
    policy::cookie_lease,
    rate::share_full,
    routes::{Form, body, field, redirect},
    store::{Digest, GrantRefusal, GrantToken, MAX_LOGIN_GRANTS, State, credentials, digest},
};
use crate::headers::{SEC_FETCH_DEST, SEC_FETCH_MODE, SEC_FETCH_SITE};
use crate::{
    app::{finalize::Access, query, response},
    lock,
    peer::{ClientKey, ClientKeys, Peer},
    transport::body::Body,
};
use graphite_meter_proto::approval::{challenge, verification_code};
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header};
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;

const LIFETIME: Duration = Duration::from_mins(2);
/// How long an issued grant answers its verifier again, for an exchange whose answer was lost.
const REPLAY: Duration = Duration::from_secs(30);
const MAX_APPROVALS: usize = 256;
const MAX_LOGIN_APPROVALS: usize = 8;
const MAX_CLIENT_APPROVALS: usize = 8;
const MAX_VERIFIER: usize = 128;
const MIN_BROWSER_VERIFIER: usize = 32;

/// An approval challenge, checked once: an S256 digest, with the code the operator compares it by.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Challenge {
    text: String,
    code: String,
}

impl Challenge {
    pub(super) fn parse(text: &str) -> Option<Self> {
        Some(Self { code: verification_code(text)?, text: text.to_owned() })
    }

    /// The challenge `verifier` answers.
    fn of(verifier: &str) -> Option<Self> {
        Self::parse(&challenge(verifier))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.text
    }
}

/// An approval of one challenge: pending, then the grant it issued while that lasts.
pub(super) struct Approval {
    /// The browser origin asking, or none for a terminal.
    browser: Option<BrowserOrigin>,
    clients: Vec<ClientKey>,
    stage: Stage,
    pub deadline: Instant,
}

/// How far an approval is: unclaimed, claimed by the login that may approve it, approved by that login, or the grant
/// it issued.
#[derive(Clone)]
enum Stage {
    Open,
    Claimed(LoginKey),
    Approved(LoginKey),
    Issued(Issued),
}

/// The grant an approval issued, and its token while the verifier may ask again.
#[derive(Clone)]
struct Issued {
    login: LoginKey,
    grant: Digest,
    token: Option<GrantToken>,
    replay_until: Instant,
}

impl Approval {
    /// The login that claimed it.
    pub(super) fn login(&self) -> Option<LoginKey> {
        match &self.stage {
            Stage::Open => None,
            Stage::Claimed(login) | Stage::Approved(login) => Some(*login),
            Stage::Issued(issued) => Some(issued.login),
        }
    }

    fn issued(&self) -> bool {
        matches!(self.stage, Stage::Issued(_))
    }

    /// Whether it issued the grant keyed `grant`.
    pub(super) fn became(&self, grant: &Digest) -> bool {
        matches!(&self.stage, Stage::Issued(issued) if issued.grant == *grant)
    }

    /// Drops an issued token whose replay window passed by `now`.
    pub(super) fn forget_token(&mut self, now: Instant) {
        if let Stage::Issued(issued) = &mut self.stage
            && now >= issued.replay_until
        {
            issued.token = None;
        }
    }
}

/// Room for one more approval: within its login's eight, and within the server bounds and the client's share.
struct Room {
    login: bool,
    shared: bool,
}

impl State {
    /// The room for one more approval of `login`; before sign-in at most half the server's approvals.
    fn approval_room(&self, login: Option<LoginKey>, keys: &ClientKeys) -> Room {
        // An issued approval is bounded by its live grant, so only pending ones count.
        let count = |held: &dyn Fn(&Approval) -> bool| {
            let pending = self.approvals.values().filter(|approval| !approval.issued());
            pending.filter(|approval| held(approval)).count()
        };
        let held = count(&|approval| approval.login() == login);
        let client = |key: &ClientKey| count(&|approval| approval.clients.contains(key));
        let total = count(&|_| true) < MAX_APPROVALS && (login.is_some() || held < MAX_APPROVALS / 2);
        Room {
            login: held < MAX_LOGIN_APPROVALS,
            shared: total && !share_full(keys, MAX_CLIENT_APPROVALS, client),
        }
    }

    /// The approval of `challenge` while it lasts.
    fn pending(&self, challenge: &Challenge) -> Option<&Approval> {
        self.approvals
            .get(challenge)
            .filter(|approval| Instant::now() < approval.deadline)
    }

    fn open(
        &mut self,
        challenge: Challenge,
        browser: Option<BrowserOrigin>,
        keys: &ClientKeys,
        login: Option<LoginKey>,
    ) {
        let (clients, deadline) = (keys.iter().collect(), Instant::now() + LIFETIME);
        let stage = login.map_or(Stage::Open, Stage::Claimed);
        self.approvals
            .insert(challenge, Approval { browser, clients, stage, deadline });
    }
}

/// `GET /auth/cli`: the signed-in login's approval page for a terminal's challenge.
pub(super) fn cli_page<B>(auth: &Enabled, request: &Request<B>, peer: &Peer) -> Response<Body> {
    let challenge = query::get(request.uri().query(), "challenge").unwrap_or_default();
    let Some(challenge) = Challenge::parse(&challenge) else {
        return refused(Refused::Link);
    };
    let lease = signed_in(auth, request.headers());
    let mut state = lock(&auth.store.0);
    let found = state.pending(&challenge);
    let found = found.map(|approval| (approval.browser.clone(), approval.login(), approval.issued()));
    if let Some((Some(origin), ..)) = &found {
        let origin = origin.header().to_str().unwrap_or_default();
        let query = query::encode(&[("challenge", challenge.as_str()), ("client_origin", origin)]);
        return redirect(&format!("/auth/browser?{query}"));
    }
    let Some(lease) = lease else {
        return redirect(&format!("/login?challenge={}", challenge.as_str()));
    };
    let Some(keys) = peer.keys() else { return refused(Refused::Busy) };
    match found {
        Some((_, login, issued)) if issued || login != Some(lease.login()) => return refused(Refused::Link),
        Some(_) => {}
        None => {
            state.sweep(Instant::now());
            let room = state.approval_room(Some(lease.login()), &keys);
            if !(room.login && room.shared) {
                drop(state);
                auth.security.count(Counter::Capacity);
                return refused(Refused::Busy);
            }
            state.open(challenge.clone(), None, &keys, Some(lease.login()));
        }
    }
    drop(state);
    approval_page(auth, &lease, &challenge, None)
}

/// `GET /auth/browser`: the approval page for a browser origin's challenge, opened before sign-in when need be.
pub(super) fn browser_page<B>(auth: &Enabled, request: &Request<B>, peer: &Peer) -> Response<Body> {
    let (headers, query) = (request.headers(), request.uri().query());
    let challenge = query::get(query, "challenge").and_then(|challenge| Challenge::parse(&challenge));
    let origin = query::get(query, "client_origin").and_then(|origin| HeaderValue::from_str(&origin).ok());
    let (Some(challenge), Some(origin)) = (challenge, origin.as_ref().and_then(BrowserOrigin::parse)) else {
        return refused(Refused::Link);
    };
    let known = lock(&auth.store.0).pending(&challenge).is_some();
    let keys = peer
        .keys()
        .filter(|keys| known || auth.browser_approvals.allow(keys, false, None));
    let Some(keys) = keys else { return refused(Refused::Busy) };
    let login = signed_in(auth, headers);
    let mut state = lock(&auth.store.0);
    if state.pending(&challenge).is_none() {
        state.sweep(Instant::now());
        if !state.approval_room(login.as_ref().map(AuthLease::login), &keys).shared {
            return refused(Refused::Busy);
        }
        state.open(challenge.clone(), Some(origin.clone()), &keys, None);
    }
    let room = login
        .as_ref()
        .map(|lease| state.approval_room(Some(lease.login()), &keys).login);
    let Some(approval) = state.approvals.get_mut(&challenge) else {
        return refused(Refused::Link);
    };
    if approval.browser.as_ref() != Some(&origin) || approval.issued() {
        return refused(Refused::Link);
    }
    let Some(lease) = login else {
        drop(state);
        let opening = page::Continue { challenge: Some(challenge.as_str()), opening: true };
        return match cross_site_navigation(headers) {
            true => response::html(opening.render()),
            false => redirect(&format!("/login?challenge={}", challenge.as_str())),
        };
    };
    match approval.stage {
        Stage::Open if room == Some(true) => approval.stage = Stage::Claimed(lease.login()),
        Stage::Open => return refused(Refused::Busy),
        Stage::Claimed(login) | Stage::Approved(login) if login != lease.login() => return refused(Refused::Link),
        Stage::Claimed(_) | Stage::Approved(_) => {}
        Stage::Issued(_) => return refused(Refused::Link),
    }
    let full = state.grants_of(lease.login()) >= MAX_LOGIN_GRANTS;
    drop(state);
    if full {
        return capacity();
    }
    approval_page(auth, &lease, &challenge, Some(&origin))
}

/// `POST /auth/{cli,browser}/approve`: the login's approval of its pending challenge, proving its CSRF token.
pub(super) fn approve(auth: &Enabled, lease: Option<&AuthLease>, form: Option<Form>, browser: bool) -> Response<Body> {
    let (Some(lease), Some(form)) = (lease, form) else {
        return response::empty(StatusCode::FORBIDDEN);
    };
    if !auth.store.csrf(lease.login(), field(&form, "csrf")) {
        return response::empty(StatusCode::FORBIDDEN);
    }
    let Some(challenge) = Challenge::parse(field(&form, "challenge")) else {
        return response::empty(StatusCode::FORBIDDEN);
    };
    let mut state = lock(&auth.store.0);
    let full = browser && state.grants_of(lease.login()) >= MAX_LOGIN_GRANTS;
    let now = Instant::now();
    let pending = state.approvals.get_mut(&challenge);
    let Some(approval) = pending.filter(|approval| {
        now < approval.deadline
            && approval.login() == Some(lease.login())
            && approval.browser.is_some() == browser
            && !approval.issued()
    }) else {
        return response::empty(StatusCode::FORBIDDEN);
    };
    if full {
        return capacity();
    }
    approval.stage = Stage::Approved(lease.login());
    drop(state);
    auth.security.count(Counter::CliApproval);
    crate::log!(Info, "auth", "sign-in approved for a {}", if browser { "browser" } else { "terminal" });
    response::html(page::CliDone { browser }.render())
}

/// `POST /auth/{cli,browser}/token`: the grant for an approved challenge's verifier; a browser's from its origin.
pub(super) async fn token<B: http_body::Body>(auth: &Enabled, request: Request<B>, browser: bool) -> Response<Body> {
    #[derive(Deserialize)]
    struct Exchange {
        verifier: String,
    }
    let origin = request.headers().get(header::ORIGIN).and_then(BrowserOrigin::parse);
    let origin = origin.filter(|_| browser);
    if browser && origin.is_none() {
        return response::empty(StatusCode::FORBIDDEN);
    }
    let document = body(request.into_body()).await;
    let verifier = document.and_then(|document| serde_json::from_slice::<Exchange>(&document).ok());
    let shortest = if browser { MIN_BROWSER_VERIFIER } else { 0 };
    let verifier = verifier.filter(|exchange| (shortest..=MAX_VERIFIER).contains(&exchange.verifier.len()));
    let mut answer = match verifier {
        Some(exchange) => issue(auth, &exchange.verifier, origin.clone()),
        None if browser => response::empty(StatusCode::FORBIDDEN),
        None => pending(),
    };
    if let Some(origin) = origin {
        Access::Bearer(origin.header().clone()).apply(answer.headers_mut());
    }
    answer
}

/// Issues the grant an approved challenge of `verifier` holds for `origin`, the same one again for 30 s.
fn issue(auth: &Enabled, verifier: &str, origin: Option<BrowserOrigin>) -> Response<Body> {
    let (Some(challenge), credentials) = (Challenge::of(verifier), credentials()) else {
        return pending();
    };
    let (mut state, now) = (lock(&auth.store.0), Instant::now());
    state.sweep(now);
    let approval = state.pending(&challenge).filter(|approval| approval.browser == origin);
    let Some(stage) = approval.map(|approval| approval.stage.clone()) else {
        return pending();
    };
    let Some(login) = approval.and_then(Approval::login) else {
        return pending();
    };
    let Some(expires) = state.login(&login.0).map(|login| login.expires) else {
        return pending();
    };
    let token = match stage {
        // A lost answer is asked again with the same verifier, so it gets the same grant for a short while.
        Stage::Issued(Issued { token: Some(token), .. }) => token,
        Stage::Issued(_) => return response::empty(StatusCode::GONE),
        Stage::Open | Stage::Claimed(_) => {
            let full = origin.is_some() && state.grants_of(login) >= MAX_LOGIN_GRANTS;
            return if full { response::empty(StatusCode::TOO_MANY_REQUESTS) } else { pending() };
        }
        Stage::Approved(_) => {
            let token = match state.grant(login, origin.clone(), credentials) {
                Ok(token) => token,
                Err(GrantRefusal::Full) => return response::empty(StatusCode::TOO_MANY_REQUESTS),
                Err(GrantRefusal::NoLogin) => return pending(),
            };
            let (replay_until, grant) = (now + REPLAY, digest(token.as_str()));
            if let Some(approval) = state.approvals.get_mut(&challenge) {
                approval.deadline = approval.deadline.max(replay_until);
                approval.stage = Stage::Issued(Issued { login, grant, token: Some(token.clone()), replay_until });
            }
            token
        }
    };
    drop(state);
    let remaining = expires.duration_since(SystemTime::now()).unwrap_or_default();
    response::json_of(&match origin {
        None => json!({"token": token.as_str(), "expires": crate::clock::rfc3339(expires)}),
        Some(_) => json!({
            "token": token.as_str(),
            "expires": expires.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            "remainingMs": remaining.as_millis(),
            "maximumLifetimeMs": LOGIN_LIFETIME.as_millis(),
        }),
    })
}

/// A document navigation from another site, which reenters with the strictly same-site session cookie.
fn cross_site_navigation(headers: &HeaderMap) -> bool {
    let sent = |name, value| headers.get(name).is_some_and(|given| given == value);
    sent(SEC_FETCH_SITE, "cross-site") && sent(SEC_FETCH_MODE, "navigate") && sent(SEC_FETCH_DEST, "document")
}

/// The cookie login of a request that carries no `Authorization`.
fn signed_in(auth: &Enabled, headers: &HeaderMap) -> Option<AuthLease> {
    if headers.contains_key(header::AUTHORIZATION) {
        return None;
    }
    cookie_lease(&auth.store, headers)
}

/// The page approving `challenge` for a terminal, or for the browser `origin`.
fn approval_page(
    auth: &Enabled,
    lease: &AuthLease,
    challenge: &Challenge,
    origin: Option<&BrowserOrigin>,
) -> Response<Body> {
    let Some(login) = auth.store.view(lease.login()) else {
        return refused(Refused::Link);
    };
    let browser_origin = origin
        .and_then(|origin| origin.header().to_str().ok())
        .unwrap_or_default();
    let csrf = login.csrf.as_str();
    let page = page::Cli::Approval {
        code: &challenge.code,
        challenge: challenge.as_str(),
        csrf,
        browser_origin,
    };
    response::html(page.render())
}

graphite_meter_proto::table! {
    /// Why the approval page refuses: a link it cannot approve, or bounds spent for now.
    enum Refused {
        name.0: &'static str,
    } {
        Link => ("link",),
        Busy => ("busy",),
    }
}

fn refused(refused: Refused) -> Response<Body> {
    let page = page::Cli::Refused(refused.name()).render();
    with_status(StatusCode::FORBIDDEN, response::html(page))
}

/// The page of a login holding its eight grants.
fn capacity() -> Response<Body> {
    let page = page::Cli::Capacity { limit: MAX_LOGIN_GRANTS }.render();
    with_status(StatusCode::TOO_MANY_REQUESTS, response::html(page))
}

fn pending() -> Response<Body> {
    with_status(StatusCode::ACCEPTED, response::json(r#"{"status":"pending"}"#))
}

fn with_status(status: StatusCode, mut answer: Response<Body>) -> Response<Body> {
    *answer.status_mut() = status;
    answer
}

//! Sign-in approvals: a terminal or browser asks with a challenge, the operator approves, the verifier earns a grant.

use super::{
    AuthLease, Enabled, LOGIN_LIFETIME, LoginKey,
    page::{self, Page},
    policy::{browser_origin, cookie_lease},
    rate::share_full,
    routes::{Form, body, field, redirect},
    security::Counter,
    store::{GrantRefusal, MAX_LOGIN_GRANTS, State, credentials},
};
use crate::{
    app::{finalize::Access, query, response},
    lock,
    log::rfc3339,
    peer::{ClientKey, ClientKeys, Peer},
    transport::body::Body,
};
use graphite_meter_proto::approval::{challenge, verification_code};
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header};
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;

const LIFETIME: Duration = Duration::from_secs(2 * 60);
const MAX_APPROVALS: usize = 256;
const MAX_LOGIN_APPROVALS: usize = 8;
const MAX_CLIENT_APPROVALS: usize = 8;
const MAX_VERIFIER: usize = 128;
const MIN_BROWSER_VERIFIER: usize = 32;

/// A pending approval of one challenge.
pub(super) struct Approval {
    /// The browser origin asking, or none for a terminal.
    browser: Option<HeaderValue>,
    clients: Vec<ClientKey>,
    /// The login approving it, once known.
    pub login: Option<LoginKey>,
    pub deadline: Instant,
    approved: bool,
}

impl State {
    /// Whether one more approval fits `login`'s eight, the client's share and server bounds; half at most pre-sign-in.
    fn approval_room(&self, login: Option<LoginKey>, keys: &ClientKeys) -> (bool, bool) {
        let count =
            |held: &dyn Fn(&Approval) -> bool| self.approvals.values().filter(|approval| held(approval)).count();
        let held = count(&|approval| approval.login == login);
        let client = |key: &ClientKey| count(&|approval| approval.clients.contains(key));
        let total = self.approvals.len() < MAX_APPROVALS && (login.is_some() || held < MAX_APPROVALS / 2);
        (held < MAX_LOGIN_APPROVALS, total && !share_full(keys, MAX_CLIENT_APPROVALS, client))
    }

    /// The approval of `challenge` while it lasts.
    fn pending(&self, challenge: &str) -> Option<&Approval> {
        self.approvals
            .get(challenge)
            .filter(|approval| Instant::now() < approval.deadline)
    }

    fn open(&mut self, challenge: &str, browser: Option<HeaderValue>, keys: &ClientKeys, login: Option<LoginKey>) {
        let (clients, deadline) = (keys.iter().collect(), Instant::now() + LIFETIME);
        let approval = Approval { browser, clients, login, deadline, approved: false };
        self.approvals.insert(challenge.to_owned(), approval);
    }
}

/// `GET /auth/cli`: the signed-in login's approval page for a terminal's challenge.
pub(super) fn cli_page<B>(auth: &Enabled, request: &Request<B>, peer: &Peer) -> Response<Body> {
    let challenge = query::get(request.uri().query(), "challenge").unwrap_or_default();
    if verification_code(&challenge).is_none() {
        return refused(false);
    }
    let lease = signed_in(auth, request.headers());
    let mut state = lock(&auth.store.0);
    let found = state.pending(&challenge);
    let found = found.map(|approval| (approval.browser.clone(), approval.login));
    if let Some((Some(origin), _)) = &found {
        let origin = origin.to_str().unwrap_or_default();
        let query = query::encode(&[("challenge", &challenge), ("client_origin", origin)]);
        return redirect(&format!("/auth/browser?{query}"));
    }
    let Some(lease) = lease else {
        return redirect(&format!("/login?challenge={challenge}"));
    };
    let Some(keys) = peer.keys() else { return refused(true) };
    match found {
        Some((_, login)) if login != Some(lease.login()) => return refused(false),
        Some(_) => {}
        None => {
            state.sweep(Instant::now());
            if state.approval_room(Some(lease.login()), &keys) != (true, true) {
                drop(state);
                auth.security.count(Counter::Capacity);
                return refused(true);
            }
            state.open(&challenge, None, &keys, Some(lease.login()));
        }
    }
    drop(state);
    approval_page(auth, &lease, &challenge, None)
}

/// `GET /auth/browser`: the approval page for a browser origin's challenge, opened before sign-in when need be.
pub(super) fn browser_page<B>(auth: &Enabled, request: &Request<B>, peer: &Peer) -> Response<Body> {
    let (headers, query) = (request.headers(), request.uri().query());
    let challenge = query::get(query, "challenge").unwrap_or_default();
    let origin = query::get(query, "client_origin").and_then(|origin| HeaderValue::from_str(&origin).ok());
    let Some(origin) = origin.filter(|origin| browser_origin(origin) && verification_code(&challenge).is_some()) else {
        return refused(false);
    };
    let known = lock(&auth.store.0).pending(&challenge).is_some();
    let keys = peer
        .keys()
        .filter(|keys| known || auth.browser_approvals.allow(keys, false, None));
    let Some(keys) = keys else { return refused(true) };
    let login = signed_in(auth, headers);
    let mut state = lock(&auth.store.0);
    if state.pending(&challenge).is_none() {
        state.sweep(Instant::now());
        if !state.approval_room(login.as_ref().map(AuthLease::login), &keys).1 {
            return refused(true);
        }
        state.open(&challenge, Some(origin.clone()), &keys, None);
    }
    let approval = &state.approvals[&challenge];
    let (asked, holder) = (approval.browser.as_ref() == Some(&origin), approval.login);
    if !asked {
        return refused(false);
    }
    let Some(lease) = login else {
        drop(state);
        return match cross_site_navigation(headers) {
            true => response::html(page::render(Page::Continue, &[("Challenge", &challenge), ("Opening", "true")])),
            false => redirect(&format!("/login?challenge={challenge}")),
        };
    };
    if holder != Some(lease.login()) {
        if holder.is_some() {
            return refused(false);
        }
        if !state.approval_room(Some(lease.login()), &keys).0 {
            return refused(true);
        }
        state.approvals.get_mut(&challenge).expect("the approval exists").login = Some(lease.login());
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
    let mut state = lock(&auth.store.0);
    let full = browser && state.grants_of(lease.login()) >= MAX_LOGIN_GRANTS;
    let now = Instant::now();
    let pending = state.approvals.get_mut(field(&form, "challenge"));
    let Some(approval) = pending.filter(|approval| {
        now < approval.deadline && approval.login == Some(lease.login()) && approval.browser.is_some() == browser
    }) else {
        return response::empty(StatusCode::FORBIDDEN);
    };
    if full {
        return capacity();
    }
    approval.approved = true;
    drop(state);
    auth.security.count(Counter::CliApproval);
    response::html(page::render(Page::CliDone, &[("Browser", if browser { "true" } else { "" })]))
}

/// `POST /auth/{cli,browser}/token`: the grant for an approved challenge's verifier; a browser's from its origin.
pub(super) async fn token<B: http_body::Body>(auth: &Enabled, request: Request<B>, browser: bool) -> Response<Body> {
    #[derive(Deserialize)]
    struct Exchange {
        verifier: String,
    }
    let origin = request.headers().get(header::ORIGIN).cloned();
    let origin = origin.filter(|origin| browser && browser_origin(origin));
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
        Access::Bearer(origin).apply(answer.headers_mut());
    }
    answer
}

/// Issues the grant an approved challenge of `verifier` holds for `origin`, ending the approval.
fn issue(auth: &Enabled, verifier: &str, origin: Option<HeaderValue>) -> Response<Body> {
    let (challenge, credentials) = (challenge(verifier), credentials());
    let mut state = lock(&auth.store.0);
    let approval = state.pending(&challenge).filter(|approval| approval.browser == origin);
    let Some((login, approved)) = approval.and_then(|approval| Some((approval.login?, approval.approved))) else {
        return pending();
    };
    let Some(expires) = state.login(&login.0).map(|login| login.expires) else {
        return pending();
    };
    if !approved {
        let full = origin.is_some() && state.grants_of(login) >= MAX_LOGIN_GRANTS;
        return if full { response::empty(StatusCode::TOO_MANY_REQUESTS) } else { pending() };
    }
    let token = match state.grant(login, origin.clone(), credentials) {
        Ok(token) => token,
        Err(GrantRefusal::Full) => return response::empty(StatusCode::TOO_MANY_REQUESTS),
        Err(GrantRefusal::NoLogin) => return pending(),
    };
    state.approvals.remove(&challenge);
    drop(state);
    let remaining = expires.duration_since(SystemTime::now()).unwrap_or_default();
    response::json_of(&match origin {
        None => json!({"token": token, "expires": rfc3339(expires)}),
        Some(_) => json!({
            "token": token,
            "expires": expires.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
            "remainingMs": remaining.as_millis(),
            "maximumLifetimeMs": LOGIN_LIFETIME.as_millis(),
        }),
    })
}

/// A document navigation from another site, which reenters with the strictly same-site session cookie.
fn cross_site_navigation(headers: &HeaderMap) -> bool {
    let sent = |name, value| headers.get(name).is_some_and(|given| given == value);
    sent("sec-fetch-site", "cross-site") && sent("sec-fetch-mode", "navigate") && sent("sec-fetch-dest", "document")
}

/// The cookie login of a request that carries no `Authorization`.
fn signed_in(auth: &Enabled, headers: &HeaderMap) -> Option<AuthLease> {
    if headers.contains_key(header::AUTHORIZATION) {
        return None;
    }
    cookie_lease(&auth.store, headers)
}

/// The page approving `challenge` for a terminal, or for the browser `origin`.
fn approval_page(auth: &Enabled, lease: &AuthLease, challenge: &str, origin: Option<&HeaderValue>) -> Response<Body> {
    let Some(login) = auth.store.view(lease.login()) else { return refused(false) };
    let code = verification_code(challenge).unwrap_or_default();
    let origin = origin.and_then(|origin| origin.to_str().ok()).unwrap_or_default();
    let fields = [
        ("Code", code.as_str()),
        ("Challenge", challenge),
        ("CSRF", &login.csrf),
        ("BrowserOrigin", origin),
    ];
    response::html(page::render(Page::Cli, &fields))
}

/// The approval page's refusal: a link it cannot approve, or bounds spent for now.
fn refused(busy: bool) -> Response<Body> {
    let page = page::render(Page::Cli, &[("Refused", if busy { "busy" } else { "link" })]);
    with_status(StatusCode::FORBIDDEN, response::html(page))
}

/// The page of a login holding its eight grants.
fn capacity() -> Response<Body> {
    let page = page::render(Page::Cli, &[("BrowserCapacity", "true"), ("ClientLimit", &MAX_LOGIN_GRANTS.to_string())]);
    with_status(StatusCode::TOO_MANY_REQUESTS, response::html(page))
}

fn pending() -> Response<Body> {
    with_status(StatusCode::ACCEPTED, response::json(r#"{"status":"pending"}"#))
}

fn with_status(status: StatusCode, mut answer: Response<Body>) -> Response<Body> {
    *answer.status_mut() = status;
    answer
}

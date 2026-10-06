//! The store of logins, grants and one-use socket tickets; raw credentials go only to their issuer, keyed by SHA-256.

use super::{AuthLease, Holder, Via, approval::Approval};
use crate::lock;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_proto::token::SocketTicket;
use http::HeaderValue;
use sha2::{Digest as _, Sha256};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// A login lasts this long from sign-in.
pub const LOGIN_LIFETIME: Duration = Duration::from_secs(8 * 60 * 60);
const MAX_LOGINS: usize = 1024;
const MAX_SUBJECT_LOGINS: usize = 8;
pub(super) const MAX_LOGIN_GRANTS: usize = 8;
const MAX_LOGIN_TICKETS: usize = 8;
const TICKET_LIFETIME: Duration = Duration::from_secs(30);

pub(super) type Digest = [u8; 32];

/// A login's key: the digest of its session cookie.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct LoginKey(pub(super) Digest);

impl fmt::Debug for LoginKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LoginKey(..)")
    }
}

/// A new login's credentials, for its cookies.
pub struct NewLogin {
    pub key: LoginKey,
    pub token: String,
    pub csrf: String,
    pub expires: SystemTime,
}

/// Why a grant was not issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantRefusal {
    /// The login ended.
    NoLogin,
    /// The login holds its eight grants and none may be replaced.
    Full,
}

/// Why a ticket was not minted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MintRefusal {
    Ended,
    /// The login holds its eight outstanding tickets.
    Full,
}

/// What `/auth/session` reports of a login.
pub(super) struct LoginView {
    pub name: String,
    pub provider: String,
    pub expires: SystemTime,
    pub csrf: String,
}

pub(super) struct Login {
    id: Arc<str>,
    subject: Arc<str>,
    name: String,
    provider: String,
    csrf: String,
    sequence: u64,
    deadline: Instant,
    pub(super) expires: SystemTime,
    /// Cancelled when the login ends; its grants' tokens are children.
    revoked: CancellationToken,
}

struct Grant {
    login: LoginKey,
    id: Arc<str>,
    browser: Option<HeaderValue>,
    sequence: u64,
    revoked: CancellationToken,
}

struct Ticket {
    lease: AuthLease,
    target: String,
    origin: Option<HeaderValue>,
    deadline: Instant,
}

#[derive(Default)]
pub(super) struct State {
    logins: HashMap<Digest, Login>,
    grants: HashMap<Digest, Grant>,
    tickets: HashMap<Digest, Ticket>,
    /// Pending approvals by their challenge.
    pub(super) approvals: HashMap<String, Approval>,
    sequence: u64,
}

/// The authentication state every listener shares; clones share one store.
#[derive(Clone, Default)]
pub struct Store(pub(super) Arc<Mutex<State>>);

impl Store {
    /// Signs `subject` in, ending its oldest login when it holds eight; `None` when the server holds 1024.
    pub fn sign_in(&self, subject: &str, name: &str, provider: &str) -> Option<NewLogin> {
        let (token, csrf, id) = (random::<32>(), random::<32>(), random::<16>());
        let key = digest(&token);
        let now = Instant::now();
        let mut state = lock(&self.0);
        state.sweep(now);
        let held = state.held(subject);
        if held.len() >= MAX_SUBJECT_LOGINS {
            state.end(&held[0]);
        }
        if state.logins.len() >= MAX_LOGINS {
            return None;
        }
        state.sequence += 1;
        let expires = SystemTime::now() + LOGIN_LIFETIME;
        let login = Login {
            id: id.into(),
            subject: subject.into(),
            name: name.into(),
            provider: provider.into(),
            csrf: csrf.clone(),
            sequence: state.sequence,
            deadline: now + LOGIN_LIFETIME,
            expires,
            revoked: CancellationToken::new(),
        };
        state.logins.insert(key, login);
        Some(NewLogin { key: LoginKey(key), token, csrf, expires })
    }

    /// Ends the login, or with `every` its subject's logins, with grants and tickets; `false` when it already ended.
    pub fn sign_out(&self, login: LoginKey, every: bool) -> bool {
        let mut state = lock(&self.0);
        let Some(subject) = state.login(&login.0).map(|login| login.subject.clone()) else {
            return false;
        };
        let ended = if every { state.held(&subject) } else { vec![login.0] };
        for key in ended {
            state.end(&key);
        }
        true
    }

    /// The lease of the login whose session cookie is `token`.
    pub fn cookie(&self, token: &str) -> Option<AuthLease> {
        let key = digest(token);
        let mut state = lock(&self.0);
        match state.logins.get(&key) {
            Some(login) if Instant::now() < login.deadline => {
                Some(login.lease(LoginKey(key), Holder::Login(login.id.clone()), Via::Cookie, login.revoked.clone()))
            }
            Some(_) => {
                state.end(&key);
                None
            }
            None => None,
        }
    }

    /// The lease of the grant whose bearer token is `token`: 32 random bytes in canonical unpadded base64url.
    pub fn bearer(&self, token: &str) -> Option<AuthLease> {
        let mut raw = [0; 32];
        if token.len() != 43 || URL_SAFE_NO_PAD.decode_slice(token, &mut raw).ok()? != 32 {
            return None;
        }
        let state = lock(&self.0);
        let found = state.grants.get(&digest(token));
        let grant = found.filter(|grant| !grant.revoked.is_cancelled())?;
        let login = state.login(&grant.login.0)?;
        let (holder, via) = (Holder::Grant(grant.id.clone()), Via::Bearer(grant.browser.clone()));
        Some(login.lease(grant.login, holder, via, grant.revoked.clone()))
    }

    /// A test hook: issues a grant as an approved challenge does.
    pub fn grant(&self, login: LoginKey, browser: Option<HeaderValue>) -> Result<String, GrantRefusal> {
        let credentials = credentials();
        lock(&self.0).grant(login, browser, credentials)
    }

    /// A one-use ticket presenting `lease` at `target` from the `origin` that minted it, for 30 s at most.
    pub(super) fn mint(
        &self,
        lease: &AuthLease,
        target: String,
        origin: Option<HeaderValue>,
    ) -> Result<SocketTicket, MintRefusal> {
        let token = format!("gmw_{}", random::<32>());
        let now = Instant::now();
        let mut state = lock(&self.0);
        state.sweep(now);
        if lease.is_ended(now) {
            return Err(MintRefusal::Ended);
        }
        let tickets = state.tickets.values();
        let held = tickets.filter(|ticket| ticket.lease.login == lease.login);
        if held.count() >= MAX_LOGIN_TICKETS {
            return Err(MintRefusal::Full);
        }
        let deadline = (now + TICKET_LIFETIME).min(lease.expires);
        let expires = SystemTime::now() + (deadline - now);
        let lease = AuthLease { via: Via::Bearer(lease.browser().cloned()), ..lease.clone() };
        let ticket = Ticket { lease, target, origin, deadline };
        state.tickets.insert(digest(&token), ticket);
        let expires = expires.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        Ok(SocketTicket { token, expires: u64::try_from(expires).unwrap_or(u64::MAX) })
    }

    /// Spends the ticket `token`, valid when presented at its `target` from its `origin` before it expired.
    pub(super) fn redeem(&self, token: &str, target: &str, origin: Option<&HeaderValue>) -> Option<AuthLease> {
        let ticket = lock(&self.0).tickets.remove(&digest(token))?;
        let now = Instant::now();
        let valid = ticket.target == target && ticket.origin.as_ref() == origin && now < ticket.deadline;
        valid.then_some(ticket.lease).filter(|lease| !lease.is_ended(now))
    }

    /// Whether `proof` is the login's CSRF token, compared in constant time.
    pub(super) fn csrf(&self, login: LoginKey, proof: &str) -> bool {
        let state = lock(&self.0);
        state
            .login(&login.0)
            .is_some_and(|login| login.csrf.as_bytes().ct_eq(proof.as_bytes()).into())
    }

    pub(super) fn view(&self, login: LoginKey) -> Option<LoginView> {
        let state = lock(&self.0);
        let login = state.login(&login.0)?;
        let (name, provider, csrf) = (login.name.clone(), login.provider.clone(), login.csrf.clone());
        Some(LoginView { name, provider, expires: login.expires, csrf })
    }
}

impl State {
    /// Issues `login` a grant with `credentials`; eight per login, then native replaces oldest native, else refused.
    pub(super) fn grant(
        &mut self,
        login: LoginKey,
        browser: Option<HeaderValue>,
        (token, id): (String, String),
    ) -> Result<String, GrantRefusal> {
        let revoked = self.login(&login.0).ok_or(GrantRefusal::NoLogin)?.revoked.child_token();
        let held: Vec<_> = self.grants.iter().filter(|(_, grant)| grant.login == login).collect();
        if held.len() >= MAX_LOGIN_GRANTS {
            let native = held.iter().filter(|(_, grant)| grant.browser.is_none());
            let oldest = native.min_by_key(|(_, grant)| grant.sequence).map(|(key, _)| **key);
            let oldest = oldest.filter(|_| browser.is_none()).ok_or(GrantRefusal::Full)?;
            if let Some(evicted) = self.grants.remove(&oldest) {
                evicted.revoked.cancel();
            }
        }
        self.sequence += 1;
        let grant = Grant {
            login,
            id: id.into(),
            browser,
            sequence: self.sequence,
            revoked,
        };
        self.grants.insert(digest(&token), grant);
        Ok(token)
    }

    /// How many grants `login` holds.
    pub(super) fn grants_of(&self, login: LoginKey) -> usize {
        self.grants.values().filter(|grant| grant.login == login).count()
    }

    /// The logins of `subject`, oldest first.
    fn held(&self, subject: &str) -> Vec<Digest> {
        let held = self.logins.iter().filter(|(_, login)| &*login.subject == subject);
        let mut held: Vec<_> = held.map(|(key, login)| (login.sequence, *key)).collect();
        held.sort_unstable();
        held.into_iter().map(|(_, key)| key).collect()
    }

    /// The login keyed `key` while it lasts.
    pub(super) fn login(&self, key: &Digest) -> Option<&Login> {
        self.logins.get(key).filter(|login| Instant::now() < login.deadline)
    }

    /// Ends the login keyed `key`, revoking every lane its grants and tickets hold.
    fn end(&mut self, key: &Digest) {
        if let Some(login) = self.logins.remove(key) {
            login.revoked.cancel();
        }
        self.grants.retain(|_, grant| grant.login.0 != *key);
        self.tickets.retain(|_, ticket| ticket.lease.login.0 != *key);
        self.approvals
            .retain(|_, approval| approval.login.is_none_or(|login| login.0 != *key));
    }

    pub(super) fn sweep(&mut self, now: Instant) {
        let expired = self.logins.iter().filter(|(_, login)| now >= login.deadline);
        for key in expired.map(|(key, _)| *key).collect::<Vec<_>>() {
            self.end(&key);
        }
        self.tickets
            .retain(|_, ticket| now < ticket.deadline && !ticket.lease.is_ended(now));
        self.approvals.retain(|_, approval| now < approval.deadline);
    }
}

impl Login {
    fn lease(&self, login: LoginKey, holder: Holder, via: Via, revoked: CancellationToken) -> AuthLease {
        let (principal, expires) = (self.subject.clone(), self.deadline);
        AuthLease { holder, principal, login, via, revoked, expires }
    }
}

pub(super) fn digest(token: &str) -> Digest {
    Sha256::digest(token).into()
}

/// `N` random bytes in unpadded base64url.
pub(super) fn random<const N: usize>() -> String {
    URL_SAFE_NO_PAD.encode(crate::random::<N>())
}

/// A new grant's bearer token and id, drawn before the store is locked.
pub(super) fn credentials() -> (String, String) {
    (random::<32>(), random::<16>())
}

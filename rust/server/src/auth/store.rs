//! The store of logins and grants; raw credentials go only to their issuer, keyed by SHA-256.

use super::{
    AuthLease, BrowserOrigin, Holder, Principal, Via,
    approval::{Approval, Challenge},
};
use crate::lock;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use subtle::ConstantTimeEq;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub const LOGIN_LIFETIME: Duration = Duration::from_hours(8);
const MAX_LOGINS: usize = 1024;
const MAX_PRINCIPAL_LOGINS: usize = 8;
pub(super) const MAX_LOGIN_GRANTS: usize = 8;

pub(super) type Digest = [u8; 32];

/// A login's key: the digest of its session cookie.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct LoginKey(pub(super) Digest);

impl fmt::Debug for LoginKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LoginKey(..)")
    }
}

/// Credentials, which never print.
macro_rules! secret {
    ($($(#[$meta:meta])* $name:ident,)+) => {$(
        $(#[$meta])*
        #[derive(Clone)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "(..)"))
            }
        }
    )+};
}

secret! {
    SessionToken,
    CsrfToken,
    GrantToken,
}

pub struct Identity<'a> {
    pub principal: Principal,
    pub name: &'a str,
    pub provider: &'a str,
}

pub struct NewLogin {
    pub key: LoginKey,
    pub token: SessionToken,
    pub csrf: CsrfToken,
    pub expires: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantRefusal {
    NoLogin,
    /// The login holds all its grants and none may be replaced.
    Full,
}

pub(super) struct LoginView {
    pub name: String,
    pub provider: String,
    pub expires: SystemTime,
    pub csrf: CsrfToken,
}

pub(super) struct Login {
    id: Arc<str>,
    principal: Principal,
    name: String,
    provider: String,
    csrf: CsrfToken,
    sequence: u64,
    deadline: Instant,
    pub(super) expires: SystemTime,
    /// Cancelled when the login ends; its grants' tokens are children.
    revoked: CancellationToken,
}

struct Grant {
    login: LoginKey,
    id: Arc<str>,
    browser: Option<BrowserOrigin>,
    sequence: u64,
    revoked: CancellationToken,
}

#[derive(Default)]
pub(super) struct State {
    logins: HashMap<Digest, Login>,
    grants: HashMap<Digest, Grant>,
    pub(super) approvals: HashMap<Challenge, Approval>,
    sequence: u64,
}

/// The authentication state every listener shares; clones share one store.
#[derive(Clone, Default)]
pub struct Store(pub(super) Arc<Mutex<State>>);

impl Store {
    /// Signs `identity` in, ending its principal's oldest login at the cap; `None` when the server is full.
    pub fn sign_in(&self, identity: Identity<'_>) -> Option<NewLogin> {
        let (token, csrf, id) = (
            SessionToken(random::<32>()),
            CsrfToken(random::<32>()),
            random::<16>(),
        );
        let key = digest(token.as_str());
        let now = Instant::now();
        let mut state = lock(&self.0);
        state.sweep(now);
        let held = state.held(&identity.principal);
        if held.len() >= MAX_PRINCIPAL_LOGINS {
            state.end(&held[0]);
        }
        if state.logins.len() >= MAX_LOGINS {
            return None;
        }
        state.sequence += 1;
        let expires = SystemTime::now() + LOGIN_LIFETIME;
        let login = Login {
            id: id.into(),
            principal: identity.principal,
            name: identity.name.into(),
            provider: identity.provider.into(),
            csrf: csrf.clone(),
            sequence: state.sequence,
            deadline: now + LOGIN_LIFETIME,
            expires,
            revoked: CancellationToken::new(),
        };
        state.logins.insert(key, login);
        Some(NewLogin {
            key: LoginKey(key),
            token,
            csrf,
            expires,
        })
    }

    /// Ends the login, or with `every` its principal's logins, with its grants; `false` when it already ended.
    pub fn sign_out(&self, login: LoginKey, every: bool) -> bool {
        let mut state = lock(&self.0);
        let Some(principal) = state.login(&login.0).map(|login| login.principal.clone()) else {
            return false;
        };
        let ended = if every {
            state.held(&principal)
        } else {
            vec![login.0]
        };
        for key in ended {
            state.end(&key);
        }
        true
    }

    pub fn cookie(&self, token: &str) -> Option<AuthLease> {
        let key = digest(token);
        let mut state = lock(&self.0);
        match state.logins.get(&key) {
            Some(login) if Instant::now() < login.deadline => Some(login.lease(
                LoginKey(key),
                Holder::Login(login.id.clone()),
                Via::Cookie,
                login.revoked.clone(),
            )),
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
        let (holder, via) = (
            Holder::Grant(grant.id.clone()),
            Via::Bearer(grant.browser.clone()),
        );
        Some(login.lease(grant.login, holder, via, grant.revoked.clone()))
    }

    pub fn grant(
        &self,
        login: LoginKey,
        browser: Option<BrowserOrigin>,
    ) -> Result<GrantToken, GrantRefusal> {
        let credentials = credentials();
        lock(&self.0).grant(login, browser, credentials)
    }

    /// Whether `proof` is the login's CSRF token, compared in constant time.
    pub(super) fn csrf(&self, login: LoginKey, proof: &str) -> bool {
        let state = lock(&self.0);
        state.login(&login.0).is_some_and(|login| {
            login
                .csrf
                .as_str()
                .as_bytes()
                .ct_eq(proof.as_bytes())
                .into()
        })
    }

    pub(super) fn view(&self, login: LoginKey) -> Option<LoginView> {
        let state = lock(&self.0);
        let login = state.login(&login.0)?;
        let (name, provider, csrf) = (
            login.name.clone(),
            login.provider.clone(),
            login.csrf.clone(),
        );
        Some(LoginView {
            name,
            provider,
            expires: login.expires,
            csrf,
        })
    }
}

impl State {
    /// Issues `login` a grant with `credentials`; past the cap native replaces oldest native, else refused.
    pub(super) fn grant(
        &mut self,
        login: LoginKey,
        browser: Option<BrowserOrigin>,
        Credentials { token, id }: Credentials,
    ) -> Result<GrantToken, GrantRefusal> {
        let revoked = self
            .login(&login.0)
            .ok_or(GrantRefusal::NoLogin)?
            .revoked
            .child_token();
        let held: Vec<_> = self
            .grants
            .iter()
            .filter(|(_, grant)| grant.login == login)
            .collect();
        if held.len() >= MAX_LOGIN_GRANTS {
            let native = held.iter().filter(|(_, grant)| grant.browser.is_none());
            let oldest = native
                .min_by_key(|(_, grant)| grant.sequence)
                .map(|(key, _)| **key);
            let oldest = oldest
                .filter(|_| browser.is_none())
                .ok_or(GrantRefusal::Full)?;
            if let Some(evicted) = self.grants.remove(&oldest) {
                evicted.revoked.cancel();
            }
            self.approvals
                .retain(|_, approval| !approval.became(&oldest));
        }
        self.sequence += 1;
        let grant = Grant {
            login,
            id,
            browser,
            sequence: self.sequence,
            revoked,
        };
        self.grants.insert(digest(token.as_str()), grant);
        Ok(token)
    }

    pub(super) fn grants_of(&self, login: LoginKey) -> usize {
        self.grants
            .values()
            .filter(|grant| grant.login == login)
            .count()
    }

    fn held(&self, principal: &Principal) -> Vec<Digest> {
        let held = self
            .logins
            .iter()
            .filter(|(_, login)| login.principal == *principal);
        let mut held: Vec<_> = held.map(|(key, login)| (login.sequence, *key)).collect();
        held.sort_unstable();
        held.into_iter().map(|(_, key)| key).collect()
    }

    pub(super) fn login(&self, key: &Digest) -> Option<&Login> {
        self.logins
            .get(key)
            .filter(|login| Instant::now() < login.deadline)
    }

    /// Ends the login keyed `key`, revoking every lane its grants hold.
    fn end(&mut self, key: &Digest) {
        if let Some(login) = self.logins.remove(key) {
            login.revoked.cancel();
        }
        self.grants.retain(|_, grant| grant.login.0 != *key);
        self.approvals
            .retain(|_, approval| approval.login().is_none_or(|login| login.0 != *key));
    }

    pub(super) fn sweep(&mut self, now: Instant) {
        let expired = self
            .logins
            .iter()
            .filter(|(_, login)| now >= login.deadline);
        for key in expired.map(|(key, _)| *key).collect::<Vec<_>>() {
            self.end(&key);
        }
        self.approvals.retain(|_, approval| {
            approval.forget_token(now);
            now < approval.deadline
        });
    }
}

impl Login {
    fn lease(
        &self,
        login: LoginKey,
        holder: Holder,
        via: Via,
        revoked: CancellationToken,
    ) -> AuthLease {
        let (principal, expires) = (self.principal.clone(), self.deadline);
        AuthLease {
            holder,
            principal,
            login,
            via,
            revoked,
            expires,
        }
    }
}

pub(crate) fn digest(token: &str) -> Digest {
    let digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
    digest
        .as_ref()
        .try_into()
        .expect("SHA-256 digests are 32 bytes")
}

pub(crate) fn random<const N: usize>() -> String {
    URL_SAFE_NO_PAD.encode(crate::random::<N>())
}

pub(super) struct Credentials {
    token: GrantToken,
    id: Arc<str>,
}

/// A new grant's credentials, drawn before the store is locked.
pub(super) fn credentials() -> Credentials {
    Credentials {
        token: GrantToken(random::<32>()),
        id: random::<16>().into(),
    }
}

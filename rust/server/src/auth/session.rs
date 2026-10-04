use super::{approval::Approval, grant::AuthLease, ticket::StoredTicket};
use crate::sync::lock;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tokio::{sync::watch, time::Instant};

pub const SESSION_LIFETIME: Duration = Duration::from_secs(8 * 60 * 60);
const MAX_SESSIONS: usize = 1024;
const MAX_SUBJECT_SESSIONS: usize = 8;
pub(super) type TokenHash = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    Capacity,
    RandomUnavailable,
}

/// Session metadata excludes the bearer token. No Debug implementation: CSRF is secret.
pub struct Session {
    pub(super) hash: TokenHash,
    pub id: String,
    pub subject: String,
    pub name: String,
    pub provider: String,
    pub csrf: String,
    pub expires: SystemTime,
    created: Instant,
    pub(super) deadline: Instant,
    revoked: watch::Sender<bool>,
}

impl Session {
    pub(super) fn active_at(&self, now: Instant) -> bool {
        now < self.deadline && !*self.revoked.borrow()
    }

    fn revoke(&self) {
        // Retain the value even when no waiter exists yet.
        self.revoked.send_replace(true);
    }
}

/// Owned by requests, streaming bodies and upgraded transports until their work ends.
/// Dropping a lease does not revoke the login.
#[derive(Clone)]
pub struct SessionLease(pub(super) Arc<Session>);

impl SessionLease {
    pub fn session(&self) -> &Session {
        &self.0
    }

    pub fn is_active(&self) -> bool {
        self.0.active_at(Instant::now())
    }

    /// Completes on logout, rotation, eviction, store shutdown or absolute expiry.
    /// Safe to call after revocation and to cancel/recreate inside a select loop.
    pub async fn ended(&self) {
        let mut revoked = self.0.revoked.subscribe();
        if *revoked.borrow_and_update() || Instant::now() >= self.0.deadline {
            return;
        }
        tokio::select! {
            _ = revoked.changed() => {},
            _ = tokio::time::sleep_until(self.0.deadline) => {},
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    pub(super) grant_sequence: u64,
    pub(super) sessions: HashMap<TokenHash, Arc<Session>>,
    pub(super) grants: HashMap<TokenHash, AuthLease>,
    pub(super) tickets: HashMap<TokenHash, StoredTicket>,
    pub(super) approvals: HashMap<String, Approval>,
}

impl State {
    pub(super) fn remove(&mut self, key: &TokenHash) -> bool {
        let Some(session) = self.sessions.remove(key) else {
            return false;
        };
        session.revoke();
        self.reap_delegated(Instant::now());
        true
    }

    pub(super) fn contains(&self, lease: &SessionLease) -> bool {
        self.sessions
            .get(&lease.0.hash)
            .is_some_and(|stored| Arc::ptr_eq(stored, &lease.0))
    }

    pub(super) fn remove_grant(&mut self, hash: &TokenHash) -> bool {
        let Some(grant) = self.grants.remove(hash) else {
            return false;
        };
        grant.revoke_grant();
        self.reap_delegated(Instant::now());
        true
    }

    pub(super) fn reap_delegated(&mut self, now: Instant) {
        self.grants.retain(|_, grant| grant.active_at(now));
        self.tickets.retain(|_, ticket| ticket.active_at(now));
        self.approvals.retain(|_, approval| approval.active_at(now));
    }

    pub(super) fn sweep(&mut self, now: Instant) {
        for (_, session) in self.sessions.extract_if(|_, session| !session.active_at(now)) {
            session.revoke();
        }
        self.reap_delegated(now);
    }

    pub(super) fn revoke_subject(&mut self, subject: &str) {
        for (_, session) in self.sessions.extract_if(|_, session| session.subject == subject) {
            session.revoke();
        }
        self.reap_delegated(Instant::now());
    }
}

impl Drop for State {
    fn drop(&mut self) {
        for session in self.sessions.values() {
            session.revoke();
        }
    }
}

/// Owns hashed credentials; clones share a single capacity and revocation domain.
#[derive(Clone, Default)]
pub struct SessionStore(pub(super) Arc<Mutex<State>>);

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the raw cookie token only to its issuer. Successful sign-in revokes
    /// the supplied previous cookie after creating its replacement.
    pub fn create(
        &self,
        subject: &str,
        name: &str,
        provider: &str,
        prior_token: Option<&str>,
    ) -> Result<(String, SessionLease), SessionError> {
        let raw = random_token::<32>()?;
        let hash = token_hash(&raw);
        let (revoked, _) = watch::channel(false);
        let now = Instant::now();
        let session = Arc::new(Session {
            hash,
            id: random_token::<16>()?,
            csrf: random_token::<32>()?,
            subject: subject.into(),
            name: name.into(),
            provider: provider.into(),
            expires: SystemTime::now() + SESSION_LIFETIME,
            created: now,
            deadline: now + SESSION_LIFETIME,
            revoked,
        });
        let mut state = lock(&self.0);
        state.sweep(now);
        let same_subject = state.sessions.values().filter(|session| session.subject == subject);
        let oldest = same_subject
            .clone()
            .min_by_key(|session| session.created)
            .map(|session| session.hash);
        if same_subject.count() >= MAX_SUBJECT_SESSIONS {
            state.remove(&oldest.expect("subject at capacity has a session"));
        }
        // Go evicts the oldest same-subject session before checking global capacity.
        if state.sessions.len() >= MAX_SESSIONS {
            return Err(SessionError::Capacity);
        }
        state.sessions.insert(hash, session.clone());
        if let Some(prior) = prior_token.map(token_hash).filter(|prior| *prior != hash) {
            state.remove(&prior);
        }
        Ok((raw, SessionLease(session)))
    }

    pub fn lookup(&self, token: &str) -> Option<SessionLease> {
        let hash = token_hash(token);
        let mut state = lock(&self.0);
        let session = state.sessions.get(&hash)?;
        if !session.active_at(Instant::now()) {
            state.remove(&hash);
            return None;
        }
        Some(SessionLease(session.clone()))
    }

    pub fn revoke(&self, lease: &SessionLease) -> bool {
        let mut state = lock(&self.0);
        state.contains(lease) && state.remove(&lease.0.hash)
    }
}

pub(super) fn token_hash(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}

pub(super) fn random_token<const N: usize>() -> Result<String, SessionError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| SessionError::RandomUnavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn expiry_is_absolute_and_lookup_revokes_without_sweep() {
        let store = SessionStore::new();
        let (token, lease) = store.create("subject", "name", "local", None).unwrap();
        tokio::time::advance(SESSION_LIFETIME - Duration::from_nanos(1)).await;
        assert!(store.lookup(&token).is_some());
        tokio::time::advance(Duration::from_nanos(1)).await;
        assert!(store.lookup(&token).is_none());
        assert!(!lease.is_active());
        lease.ended().await;
    }

    #[tokio::test(start_paused = true)]
    async fn active_transport_expires_without_lookup_or_sweep() {
        let store = SessionStore::new();
        let (_, lease) = store.create("subject", "name", "local", None).unwrap();
        let ended = tokio::time::timeout(SESSION_LIFETIME + Duration::from_secs(1), lease.ended());
        ended.await.unwrap();
        assert!(!lease.is_active());
    }
}

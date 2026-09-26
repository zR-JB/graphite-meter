use super::session::{
    Session, SessionError, SessionLease, SessionStore, State, random_token, token_hash,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_core::origin::canonical_origin;
use std::sync::Arc;
use tokio::{sync::watch, time::Instant};

pub(super) const MAX_SESSION_GRANTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantError {
    NoSession,
    InvalidOrigin,
    Capacity,
    RandomUnavailable,
}

struct BrowserGrant {
    origin: String,
    id: String,
    revoked: watch::Sender<bool>,
}

/// Request identity and its owned revocation lifetime. Never logs credential material.
#[derive(Clone)]
pub struct AuthLease {
    pub(super) session: SessionLease,
    bearer: bool,
    issued: u64,
    provider: Option<&'static str>,
    browser: Option<Arc<BrowserGrant>>,
}

impl AuthLease {
    pub fn cookie(session: SessionLease) -> Self {
        Self {
            session,
            bearer: false,
            issued: 0,
            provider: None,
            browser: None,
        }
    }

    pub fn session(&self) -> &Session {
        self.session.session()
    }
    pub fn is_bearer(&self) -> bool {
        self.bearer
    }
    pub fn browser_origin(&self) -> Option<&str> {
        self.browser.as_ref().map(|grant| grant.origin.as_str())
    }
    pub fn provider(&self) -> &str {
        self.provider.unwrap_or(self.session().provider())
    }
    pub fn owner(&self) -> crate::upload::Owner {
        match &self.browser {
            Some(grant) => crate::upload::Owner::delegated(self.session().subject(), &grant.id),
            None => crate::upload::Owner::principal(self.session().subject()),
        }
    }
    pub fn is_active(&self) -> bool {
        self.active_at(Instant::now())
    }
    pub(super) fn active_at(&self, now: Instant) -> bool {
        self.session.0.active_at(now)
            && self
                .browser
                .as_ref()
                .is_none_or(|grant| !*grant.revoked.borrow())
    }
    pub(super) fn revoke_grant(&self) {
        if let Some(grant) = &self.browser {
            grant.revoked.send_replace(true);
        }
    }
    pub(super) fn as_ticket(&self) -> Self {
        let mut lease = self.clone();
        lease.bearer = true;
        lease
    }

    /// Browser grant eviction ends its active work; CLI grant eviction only denies
    /// future authorization. All active work also ends with the parent session.
    pub async fn ended(&self) {
        let Some(grant) = &self.browser else {
            return self.session.ended().await;
        };
        let mut revoked = grant.revoked.subscribe();
        if *revoked.borrow_and_update() {
            return;
        }
        tokio::select! {
            _ = revoked.changed() => {},
            _ = self.session.ended() => {},
        }
    }
}

impl SessionStore {
    /// Called only after the CLI approval exchange has authorized this session.
    pub fn issue_cli_grant(
        &self,
        session: &SessionLease,
    ) -> Result<(String, AuthLease), GrantError> {
        self.issue_grant(session, None)
    }

    /// Called only after browser approval, with its exact canonical HTTPS audience.
    pub fn issue_browser_grant(
        &self,
        session: &SessionLease,
        origin: &str,
    ) -> Result<(String, AuthLease), GrantError> {
        if !secure_browser_origin(origin) {
            return Err(GrantError::InvalidOrigin);
        }
        self.issue_grant(session, Some(origin))
    }

    fn issue_grant(
        &self,
        session: &SessionLease,
        origin: Option<&str>,
    ) -> Result<(String, AuthLease), GrantError> {
        let mut state = self.0.lock().expect("session mutex poisoned");
        let now = Instant::now();
        state.sweep(now);
        state.issue_grant(session, origin, now)
    }

    pub fn lookup_bearer(&self, token: &str) -> Option<AuthLease> {
        self.lookup_bearer_at(token, Instant::now())
    }

    fn lookup_bearer_at(&self, token: &str, now: Instant) -> Option<AuthLease> {
        let mut decoded = [0; 32];
        // This engine rejects padding and nonzero trailing bits, so successful
        // decoding of exactly 32 bytes also establishes canonical encoding.
        if token.len() != 43 || URL_SAFE_NO_PAD.decode_slice(token, &mut decoded).ok()? != 32 {
            return None;
        }
        let key = token_hash(token);
        let mut state = self.0.lock().expect("session mutex poisoned");
        let grant = state.grants.get(&key)?;
        if grant.active_at(now) {
            return Some(grant.clone());
        }
        if !grant.session.0.active_at(now) {
            let parent = grant.session.0.hash;
            state.remove(&parent);
        } else {
            // Revoked child credentials can be removed independently. Their
            // tickets already fail closed and maintenance reclaims their slots.
            state.grants.remove(&key);
        }
        None
    }

    pub fn revoke_grant(&self, token: &str) -> bool {
        self.0
            .lock()
            .expect("session mutex poisoned")
            .remove_grant(&token_hash(token))
    }
}

impl State {
    pub(super) fn grant_count(&self, session: &SessionLease) -> usize {
        self.grants
            .values()
            .filter(|grant| grant.session.0.hash == session.0.hash)
            .count()
    }

    pub(super) fn issue_grant(
        &mut self,
        session: &SessionLease,
        origin: Option<&str>,
        now: Instant,
    ) -> Result<(String, AuthLease), GrantError> {
        if !self.contains(session) || !session.0.active_at(now) {
            return Err(GrantError::NoSession);
        }
        let evict = if self.grant_count(session) >= MAX_SESSION_GRANTS {
            if origin.is_some() {
                return Err(GrantError::Capacity);
            }
            Some(
                *self
                    .grants
                    .iter()
                    .filter(|(_, grant)| {
                        grant.session.0.hash == session.0.hash && grant.browser.is_none()
                    })
                    .min_by_key(|(_, grant)| grant.issued)
                    .map(|(key, _)| key)
                    .ok_or(GrantError::Capacity)?,
            )
        } else {
            None
        };
        // Generate before changing capacity, so RNG failure preserves current grants.
        let token = random_token::<32>().map_err(grant_random_error)?;
        let browser = if let Some(origin) = origin {
            let (revoked, _) = watch::channel(false);
            Some(Arc::new(BrowserGrant {
                origin: origin.into(),
                id: random_token::<16>().map_err(grant_random_error)?,
                revoked,
            }))
        } else {
            None
        };
        if let Some(key) = evict {
            self.remove_grant(&key);
        }
        self.grant_sequence += 1;
        let lease = AuthLease {
            session: session.clone(),
            bearer: true,
            issued: self.grant_sequence,
            provider: Some(if origin.is_some() { "browser" } else { "cli" }),
            browser,
        };
        self.grants.insert(token_hash(&token), lease.clone());
        Ok((token, lease))
    }
}

pub fn secure_browser_origin(origin: &str) -> bool {
    origin.starts_with("https://")
        && canonical_origin(origin).is_ok_and(|canonical| canonical == origin)
}

fn grant_random_error(_: SessionError) -> GrantError {
    GrantError::RandomUnavailable
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::SESSION_LIFETIME;
    use std::time::{Duration, SystemTime};

    #[test]
    fn lookup_checks_only_the_selected_grant_and_expires_its_parent_without_sweep() {
        let store = SessionStore::new();
        let now = Instant::now();
        let age = SESSION_LIFETIME - Duration::from_secs(60);
        let (_, expiring) = store
            .create_at(
                "old",
                "name",
                "local",
                None,
                SystemTime::now() - age,
                now - age,
            )
            .unwrap();
        let (expired_token, expired_lease) = store.issue_cli_grant(&expiring).unwrap();
        let (sibling_token, _) = store
            .issue_browser_grant(&expiring, "https://client.example")
            .unwrap();
        let (_, current) = store.create("current", "name", "local", None).unwrap();
        let (valid_token, _) = store.issue_cli_grant(&current).unwrap();
        let after_expiry = now + Duration::from_secs(60);

        assert!(store.lookup_bearer_at(&valid_token, after_expiry).is_some());
        assert!(
            store
                .0
                .lock()
                .unwrap()
                .grants
                .contains_key(&token_hash(&expired_token))
        );
        assert!(
            store
                .lookup_bearer_at(&expired_token, after_expiry)
                .is_none()
        );
        assert!(!expired_lease.is_active());
        assert!(store.lookup_bearer(&sibling_token).is_none());
        assert!(store.lookup_bearer(&valid_token).is_some());
        assert!(
            !store
                .0
                .lock()
                .unwrap()
                .sessions
                .contains_key(&expiring.0.hash)
        );
    }

    #[test]
    fn bearer_decoding_rejects_noncanonical_trailing_bits_without_allocation() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "name", "local", None).unwrap();
        let (token, _) = store.issue_cli_grant(&session).unwrap();
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let last = token.as_bytes()[42];
        let index = alphabet.iter().position(|byte| *byte == last).unwrap();
        let mut noncanonical = token.clone().into_bytes();
        noncanonical[42] = alphabet[index + 1];
        assert!(
            store
                .lookup_bearer(&String::from_utf8(noncanonical).unwrap())
                .is_none()
        );
        assert!(store.lookup_bearer(&token).is_some());
    }
}

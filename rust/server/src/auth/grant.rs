use super::session::{Session, SessionLease, SessionStore, State, random_token, token_hash};
use crate::{cors::Access, sync::lock};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_core::origin::canonical_origin;
use http::HeaderValue;
use std::sync::Arc;
use tokio::{sync::watch, time::Instant};

pub(super) const MAX_SESSION_GRANTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrantError {
    NoSession,
    Capacity,
    RandomUnavailable,
}

struct Grant {
    origin: Option<String>,
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
    grant: Option<Arc<Grant>>,
}

impl AuthLease {
    pub fn cookie(session: SessionLease) -> Self {
        Self {
            session,
            bearer: false,
            issued: 0,
            provider: None,
            grant: None,
        }
    }

    pub fn session(&self) -> &Session {
        self.session.session()
    }
    pub fn is_bearer(&self) -> bool {
        self.bearer
    }
    pub fn access<'a>(&self, origin: &'a HeaderValue) -> Access<'a> {
        if self.bearer { Access::Bearer(origin) } else { Access::Cookie(origin) }
    }
    pub fn browser_origin(&self) -> Option<&str> {
        self.grant.as_ref().and_then(|grant| grant.origin.as_deref())
    }
    pub fn provider(&self) -> &str {
        self.provider.unwrap_or(&self.session().provider)
    }
    pub fn owner(&self) -> crate::upload::Owner {
        match &self.grant {
            Some(grant) => crate::upload::Owner::delegated(&self.session().subject, &grant.id),
            None => crate::upload::Owner::login(&self.session().subject, &self.session().id),
        }
    }
    pub fn is_active(&self) -> bool {
        self.active_at(Instant::now())
    }
    pub(super) fn active_at(&self, now: Instant) -> bool {
        self.session.0.active_at(now) && self.grant.as_ref().is_none_or(|grant| !*grant.revoked.borrow())
    }
    pub(super) fn revoke_grant(&self) {
        if let Some(grant) = &self.grant {
            grant.revoked.send_replace(true);
        }
    }
    pub(super) fn as_ticket(&self) -> Self {
        let mut lease = self.clone();
        lease.bearer = true;
        lease
    }

    pub async fn ended(&self) {
        let Some(grant) = &self.grant else {
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
    pub fn lookup_bearer(&self, token: &str) -> Option<AuthLease> {
        let mut decoded = [0; 32];
        // This engine rejects padding and nonzero trailing bits, so successful
        // decoding of exactly 32 bytes also establishes canonical encoding.
        if token.len() != 43 || URL_SAFE_NO_PAD.decode_slice(token, &mut decoded).ok()? != 32 {
            return None;
        }
        let key = token_hash(token);
        let mut state = lock(&self.0);
        let grant = state.grants.get(&key)?;
        let now = Instant::now();
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
}

impl State {
    pub(super) fn grant_count(&self, session: &SessionLease) -> usize {
        let children = |grant: &&AuthLease| grant.session.0.hash == session.0.hash;
        self.grants.values().filter(children).count()
    }

    pub(super) fn issue_grant(
        &mut self,
        session: &SessionLease,
        origin: Option<&str>,
    ) -> Result<(String, AuthLease), GrantError> {
        if !self.contains(session) || !session.0.active_at(Instant::now()) {
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
                    .filter(|(_, grant)| grant.session.0.hash == session.0.hash && grant.browser_origin().is_none())
                    .min_by_key(|(_, grant)| grant.issued)
                    .map(|(key, _)| key)
                    .ok_or(GrantError::Capacity)?,
            )
        } else {
            None
        };
        // Generate before changing capacity, so RNG failure preserves current grants.
        let token = random_token::<32>().map_err(|_| GrantError::RandomUnavailable)?;
        let (revoked, _) = watch::channel(false);
        let grant = Some(Arc::new(Grant {
            origin: origin.map(str::to_owned),
            id: random_token::<16>().map_err(|_| GrantError::RandomUnavailable)?,
            revoked,
        }));
        if let Some(key) = evict {
            self.remove_grant(&key);
        }
        self.grant_sequence += 1;
        let lease = AuthLease {
            session: session.clone(),
            bearer: true,
            issued: self.grant_sequence,
            provider: Some(if origin.is_some() { "browser" } else { "cli" }),
            grant,
        };
        self.grants.insert(token_hash(&token), lease.clone());
        Ok((token, lease))
    }
}

pub fn secure_browser_origin(origin: &str) -> bool {
    origin.starts_with("https://") && canonical_origin(origin).is_ok_and(|canonical| canonical == origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::SESSION_LIFETIME;
    use std::time::Duration;

    const ORIGIN: &str = "https://client.example";

    /// Grants as an approved exchange issues them, and their revocation, for tests.
    impl SessionStore {
        pub(crate) fn issue_cli_grant(&self, session: &SessionLease) -> Result<(String, AuthLease), GrantError> {
            lock(&self.0).issue_grant(session, None)
        }
        pub(crate) fn issue_browser_grant(
            &self,
            session: &SessionLease,
            origin: &str,
        ) -> Result<(String, AuthLease), GrantError> {
            lock(&self.0).issue_grant(session, Some(origin))
        }
        pub(crate) fn revoke_grant(&self, token: &str) -> bool {
            lock(&self.0).remove_grant(&token_hash(token))
        }
    }

    #[test]
    fn grant_identity_preserves_parent_budget_but_isolates_browser_uploads() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "Name", "local", None).unwrap();
        let cookie = AuthLease::cookie(session.clone());
        let (cli_token, cli) = store.issue_cli_grant(&session).unwrap();
        let (first_token, first) = store.issue_browser_grant(&session, ORIGIN).unwrap();
        let (_, second) = store.issue_browser_grant(&session, ORIGIN).unwrap();
        assert_eq!(cli.provider(), "cli");
        assert_eq!(first.provider(), "browser");
        assert_eq!(cookie.provider(), "local");
        assert!(cli.is_bearer() && first.is_bearer());
        assert!(!cookie.is_bearer());
        assert_ne!(cli.owner(), cookie.owner());
        assert_eq!(cli.owner().client_keys()[1], cookie.owner().client_keys()[1]);
        assert_ne!(first.owner(), second.owner());
        assert_eq!(first.owner().client_keys()[1], cookie.owner().client_keys()[1]);
        assert_eq!(first.session().id, second.session().id);
        let found = store.lookup_bearer(&first_token).unwrap();
        assert_eq!(found.browser_origin(), Some(ORIGIN));
        assert!(store.lookup_bearer(&cli_token).is_some());
        assert!(store.lookup_bearer(&(cli_token + "=")).is_none());
    }

    #[test]
    fn capacity_preserves_browser_grants_and_evicts_only_the_oldest_native_grant() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "Name", "local", None).unwrap();
        let (first, lease) = store.issue_cli_grant(&session).unwrap();
        let (second, _) = store.issue_cli_grant(&session).unwrap();
        let browsers: Vec<_> = (0..6)
            .map(|_| store.issue_browser_grant(&session, ORIGIN).unwrap())
            .collect();
        let refused = store.issue_browser_grant(&session, ORIGIN).err();
        assert_eq!(refused, Some(GrantError::Capacity));
        let (new, _) = store.issue_cli_grant(&session).unwrap();
        assert!(store.lookup_bearer(&first).is_none() && !lease.is_active());
        assert!(store.lookup_bearer(&second).is_some() && store.lookup_bearer(&new).is_some());
        for (token, lease) in browsers {
            assert!(store.lookup_bearer(&token).is_some() && lease.is_active());
        }
        let (_, only_browsers) = store.create("subject", "Name", "local", None).unwrap();
        for _ in 0..8 {
            assert!(store.issue_browser_grant(&only_browsers, ORIGIN).is_ok());
        }
        assert_eq!(store.issue_cli_grant(&only_browsers).err(), Some(GrantError::Capacity));
    }

    #[tokio::test]
    async fn revocation_wakes_existing_waiters_and_preserves_siblings() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "Name", "local", None).unwrap();
        let (token, browser) = store.issue_browser_grant(&session, ORIGIN).unwrap();
        let (sibling_token, sibling) = store.issue_browser_grant(&session, ORIGIN).unwrap();
        let active = browser.clone();
        let waiter = tokio::spawn(async move { active.ended().await });
        tokio::task::yield_now().await;
        assert!(store.revoke_grant(&token));
        let woken = tokio::time::timeout(Duration::from_secs(1), waiter).await;
        woken.unwrap().unwrap();
        assert!(!browser.is_active() && store.lookup_bearer(&token).is_none());
        assert!(sibling.is_active() && store.lookup_bearer(&sibling_token).is_some());
        store.revoke(&session);
        let ended = tokio::time::timeout(Duration::from_secs(1), sibling.ended()).await;
        ended.unwrap();
        assert!(store.lookup_bearer(&sibling_token).is_none());
    }

    #[test]
    fn foreign_and_revoked_sessions_cannot_issue_grants() {
        let store = SessionStore::new();
        let foreign = SessionStore::new();
        let (_, session) = foreign.create("subject", "Name", "local", None).unwrap();
        assert_eq!(store.issue_cli_grant(&session).err(), Some(GrantError::NoSession));
        let refused = store.issue_browser_grant(&session, ORIGIN).err();
        assert_eq!(refused, Some(GrantError::NoSession));
        foreign.revoke(&session);
        assert_eq!(foreign.issue_cli_grant(&session).err(), Some(GrantError::NoSession));
    }

    #[test]
    fn browser_audiences_must_be_exact_canonical_https_origins() {
        for origin in [
            "http://client.example",
            "https://CLIENT.example",
            "https://client.example:443",
            "https://client.example/",
            "https://user@client.example",
            "https://client.example?",
            "null",
        ] {
            assert!(!secure_browser_origin(origin), "{origin}");
        }
        assert!(secure_browser_origin("https://client.example:8443"));
    }

    #[tokio::test(start_paused = true)]
    async fn lookup_checks_only_the_selected_grant_and_expires_its_parent_without_sweep() {
        let store = SessionStore::new();
        let (_, expiring) = store.create("old", "name", "local", None).unwrap();
        let (expired_token, expired_lease) = store.issue_cli_grant(&expiring).unwrap();
        let (sibling_token, _) = store.issue_browser_grant(&expiring, ORIGIN).unwrap();
        tokio::time::advance(SESSION_LIFETIME - Duration::from_secs(60)).await;
        let (_, current) = store.create("current", "name", "local", None).unwrap();
        let (valid_token, _) = store.issue_cli_grant(&current).unwrap();
        tokio::time::advance(Duration::from_secs(60)).await;

        assert!(store.lookup_bearer(&valid_token).is_some());
        assert!(store.0.lock().unwrap().grants.contains_key(&token_hash(&expired_token)));
        assert!(store.lookup_bearer(&expired_token).is_none());
        assert!(!expired_lease.is_active());
        assert!(store.lookup_bearer(&sibling_token).is_none());
        assert!(store.lookup_bearer(&valid_token).is_some());
        assert!(!store.0.lock().unwrap().sessions.contains_key(&expiring.0.hash));
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
        assert!(store.lookup_bearer(&String::from_utf8(noncanonical).unwrap()).is_none());
        assert!(store.lookup_bearer(&token).is_some());
    }
}

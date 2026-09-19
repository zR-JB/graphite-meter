//! Fixed-cost password verification with bounded CPU and memory concurrency.

use super::{
    SessionLease, SessionStore,
    policy::constant_equal,
    rate::{AttemptLimiter, Budget},
};
use crate::{
    config::{AuthConfig, ConfigError},
    password::Hash,
};
use std::{fs::File, io::Read, net::IpAddr, sync::Arc};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginFailure {
    Failed,
    Stale,
    Throttled,
    Busy,
    Password,
}

impl LoginFailure {
    pub const fn notice(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Stale => "stale",
            Self::Throttled => "throttled",
            Self::Busy => "busy",
            Self::Password => "password",
        }
    }
}

/// The HTTP boundary supplies the verified socket/proxy address and parsed form.
/// Intentionally no Debug: the form contains raw credentials.
pub struct PasswordAttempt<'a> {
    pub client: Option<IpAddr>,
    pub origin: &'a str,
    pub nonce_cookie: Option<&'a str>,
    pub csrf: &'a str,
    pub password: &'a str,
    pub prior_session: Option<&'a str>,
}

pub struct PasswordLogin {
    public_origin: String,
    hash: Hash,
    slots: Arc<Semaphore>,
    attempts: Arc<AttemptLimiter>,
    sessions: SessionStore,
}

impl PasswordLogin {
    pub fn new(
        config: &AuthConfig,
        sessions: SessionStore,
        attempts: Arc<AttemptLimiter>,
    ) -> Result<Self, ConfigError> {
        if !config.mode.password() {
            return Err("password authentication is disabled".into());
        }
        let encoded = read_secret(&config.password_hash, &config.password_hash_file, 4096)?;
        Ok(Self {
            public_origin: config.public_url.clone(),
            hash: Hash::parse(&encoded)?,
            slots: Arc::new(Semaphore::new(2)),
            attempts,
            sessions,
        })
    }

    pub async fn attempt(
        &self,
        attempt: PasswordAttempt<'_>,
    ) -> Result<(String, SessionLease), LoginFailure> {
        if attempt.origin.is_empty() || attempt.origin != self.public_origin {
            return Err(LoginFailure::Failed);
        }
        let nonce = attempt.nonce_cookie.ok_or(LoginFailure::Stale)?;
        if attempt.csrf.is_empty() {
            return Err(LoginFailure::Stale);
        }
        if !constant_equal(nonce, attempt.csrf) {
            return Err(LoginFailure::Failed);
        }
        let client = attempt.client.ok_or(LoginFailure::Throttled)?;
        if !self.attempts.allow(Budget::Password, client) {
            return Err(LoginFailure::Throttled);
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| LoginFailure::Busy)?;
        crate::password::validate_password(attempt.password).map_err(|_| LoginFailure::Password)?;
        let hash = self.hash.clone();
        let password = Zeroizing::new(attempt.password.to_owned());
        // The worker owns its permit. Cancelling the HTTP request cannot free a
        // slot while its uninterruptible Argon2 computation is still running.
        let verified = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            hash.verify(&password)
        })
        .await
        .map_err(|_| LoginFailure::Busy)?;
        if !verified {
            return Err(LoginFailure::Password);
        }
        // No login is issued by an abandoned worker: only this awaiting request
        // may commit the session and rotate its explicitly supplied predecessor.
        self.sessions
            .create(
                "local-operator",
                "Local operator",
                "local",
                attempt.prior_session,
            )
            .map_err(|_| LoginFailure::Busy)
    }
}

pub(super) fn read_secret(
    inline: &str,
    path: &str,
    limit: u64,
) -> Result<Zeroizing<String>, ConfigError> {
    if !inline.is_empty() {
        return Ok(Zeroizing::new(inline.trim().to_owned()));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err("secret file exceeds size limit".into());
    }
    let value = std::str::from_utf8(&bytes)?.trim();
    if value.is_empty() {
        return Err("secret is empty".into());
    }
    Ok(Zeroizing::new(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthMode;

    const GO_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0";
    const NONCE: &str = "a-valid-long-unpredictable-login-nonce";

    fn verifier(store: SessionStore) -> PasswordLogin {
        PasswordLogin::new(
            &AuthConfig {
                mode: AuthMode::Password,
                public_url: "https://meter.example".into(),
                password_hash: GO_HASH.into(),
                ..AuthConfig::default()
            },
            store,
            Arc::new(AttemptLimiter::new()),
        )
        .unwrap()
    }

    fn attempt(password: &str) -> PasswordAttempt<'_> {
        PasswordAttempt {
            client: Some("192.0.2.1".parse().unwrap()),
            origin: "https://meter.example",
            nonce_cookie: Some(NONCE),
            csrf: NONCE,
            password,
            prior_session: None,
        }
    }

    #[tokio::test]
    async fn csrf_failures_do_not_spend_password_budget_or_issue_sessions() {
        let login = verifier(SessionStore::new());
        for _ in 0..10 {
            let mut request = attempt("wrong");
            request.origin = "https://attacker.example";
            assert!(matches!(
                login.attempt(request).await,
                Err(LoginFailure::Failed)
            ));
        }
        let mut request = attempt("wrong");
        request.nonce_cookie = None;
        assert!(matches!(
            login.attempt(request).await,
            Err(LoginFailure::Stale)
        ));
        let (token, _) = login
            .attempt(attempt("correct horse battery staple"))
            .await
            .unwrap();
        assert!(login.sessions.lookup(&token).is_some());
    }

    #[tokio::test]
    async fn capacity_and_address_checks_precede_expensive_verification() {
        let login = verifier(SessionStore::new());
        let _occupied = login.slots.clone().acquire_many_owned(2).await.unwrap();
        for _ in 0..5 {
            assert!(matches!(
                login.attempt(attempt("wrong")).await,
                Err(LoginFailure::Busy)
            ));
        }
        assert!(matches!(
            login.attempt(attempt("wrong")).await,
            Err(LoginFailure::Throttled)
        ));
        let mut request = attempt("wrong");
        request.client = None;
        assert!(matches!(
            login.attempt(request).await,
            Err(LoginFailure::Throttled)
        ));
    }

    #[tokio::test]
    async fn successful_password_login_rotates_only_the_supplied_session() {
        let store = SessionStore::new();
        let login = verifier(store.clone());
        let (prior, old) = store
            .create("local-operator", "Local operator", "local", None)
            .unwrap();
        let (_, sibling) = store
            .create("local-operator", "Local operator", "local", None)
            .unwrap();
        let mut request = attempt("wrong");
        request.prior_session = Some(&prior);
        assert!(matches!(
            login.attempt(request).await,
            Err(LoginFailure::Password)
        ));
        assert!(old.is_active());
        let mut request = attempt("correct horse battery staple");
        request.prior_session = Some(&prior);
        let (_, replacement) = login.attempt(request).await.unwrap();
        assert!(!old.is_active());
        assert!(sibling.is_active() && replacement.is_active());
    }
}

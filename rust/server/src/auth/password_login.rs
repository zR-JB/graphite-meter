//! Fixed-cost password verification with bounded CPU and memory concurrency.

use super::{
    SessionLease, SessionStore,
    policy::constant_equal,
    rate::{AttemptLimiter, Budget},
    reason::Reason,
};
use crate::{
    config::{AuthConfig, ConfigError},
    password::Hash,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::{
    fs::File,
    io::Read,
    net::IpAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

const DEVICE_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);

pub fn check_csrf(public: &str, origin: &str, nonce: Option<&str>, token: &str) -> Result<(), Reason> {
    match (origin, nonce) {
        ("", _) => Err(Reason::CsrfOriginMissing),
        (origin, _) if origin != public => Err(Reason::CsrfOriginMismatch),
        (_, None) => Err(Reason::CsrfCookieMissing),
        _ if token.is_empty() => Err(Reason::CsrfTokenMissing),
        (_, Some(nonce)) if !constant_equal(nonce, token) => Err(Reason::CsrfTokenMismatch),
        _ => Ok(()),
    }
}

/// The HTTP boundary supplies the verified socket/proxy address and parsed form.
/// Intentionally no Debug: the form contains raw credentials.
pub struct PasswordAttempt<'a> {
    pub client: Option<IpAddr>,
    pub origin: &'a str,
    pub nonce_cookie: Option<&'a str>,
    pub device_cookie: Option<&'a str>,
    pub csrf: &'a str,
    pub password: &'a str,
    pub prior_session: Option<&'a str>,
}

/// The subject of every password login, and so of their grants.
pub(crate) const LOCAL_OPERATOR: &str = "local-operator";

pub struct PasswordLogin {
    public_origin: String,
    hash: Hash,
    /// Keyed by the password hash, as in Go: a device survives restarts and is forgotten with the password.
    device: Hmac<Sha256>,
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
        let encoded = read_secret("password hash", &config.password_hash, &config.password_hash_file, 4096)?;
        Ok(Self {
            public_origin: config.public_url.clone(),
            hash: Hash::parse(&encoded)?,
            device: Hmac::new_from_slice(encoded.as_bytes()).expect("HMAC accepts any key length"),
            slots: Arc::new(Semaphore::new(2)),
            attempts,
            sessions,
        })
    }

    pub async fn attempt(&self, attempt: PasswordAttempt<'_>) -> Result<(String, SessionLease), Reason> {
        check_csrf(&self.public_origin, attempt.origin, attempt.nonce_cookie, attempt.csrf)?;
        let client = attempt.client.ok_or(Reason::Throttled)?;
        let budget = if attempt.device_cookie.is_some_and(|cookie| self.known_device(cookie)) {
            Budget::KnownDevice
        } else {
            Budget::Password
        };
        if !self.attempts.allow(budget, client) {
            return Err(Reason::Throttled);
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Reason::VerifierBusy)?;
        let hash = self.hash.clone();
        let password = Zeroizing::new(attempt.password.to_owned());
        // The worker owns its permit. Cancelling the HTTP request cannot free a
        // slot while its uninterruptible Argon2 computation is still running.
        let verified = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            hash.verify(&password)
        })
        .await
        .map_err(|_| Reason::VerifierBusy)?;
        if !verified {
            self.attempts.note_failed_password();
            return Err(Reason::PasswordMismatch);
        }
        // No login is issued by an abandoned worker: only this awaiting request
        // may commit the session and rotate its explicitly supplied predecessor.
        self.sessions
            .create(LOCAL_OPERATOR, "Local operator", "local", attempt.prior_session)
            .map_err(|_| Reason::SessionCapacity)
    }

    pub fn device_cookie(&self, now: SystemTime) -> (String, SystemTime) {
        let expires = now + DEVICE_LIFETIME;
        let seconds = expires
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_be_bytes();
        let mut tag = self.device.clone();
        tag.update(&seconds);
        let value = [seconds.as_slice(), &tag.finalize().into_bytes()].concat();
        (URL_SAFE_NO_PAD.encode(value), expires)
    }

    fn known_device(&self, cookie: &str) -> bool {
        let mut raw = [0; 40];
        if !matches!(URL_SAFE_NO_PAD.decode_slice(cookie, &mut raw), Ok(40)) {
            return false;
        }
        let (expires, tag) = raw.split_at(8);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut expected = self.device.clone();
        expected.update(expires);
        now < u64::from_be_bytes(expires.try_into().expect("eight bytes")) && expected.verify_slice(tag).is_ok()
    }
}

pub(super) fn read_secret(name: &str, inline: &str, path: &str, limit: u64) -> Result<Zeroizing<String>, ConfigError> {
    if !inline.is_empty() {
        return Ok(Zeroizing::new(inline.trim().to_owned()));
    }
    let failed = |operation, error| format!("{name}: {}", crate::config::path_error(operation, path, &error));
    let mut bytes = Zeroizing::new(Vec::new());
    let file = File::open(path).map_err(|error| failed("open", error))?;
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| failed("read", error))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{name}: secret file exceeds {limit} bytes").into());
    }
    let value = std::str::from_utf8(&bytes)
        .map_err(|_| format!("{name}: secret file is not UTF-8"))?
        .trim();
    if value.is_empty() {
        return Err(format!("{name}: secret is empty").into());
    }
    Ok(Zeroizing::new(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::session::random_token, config::AuthMode, password::hash_password};

    const NONCE: &str = "a-valid-long-unpredictable-login-nonce";

    fn random_password() -> String {
        random_token::<18>().unwrap()
    }

    /// A login verifier for a fresh random password, returned alongside it.
    fn verifier(store: SessionStore) -> (PasswordLogin, String) {
        let password = random_password();
        let login = PasswordLogin::new(
            &AuthConfig {
                mode: AuthMode::Password,
                public_url: "https://meter.example".into(),
                password_hash: hash_password(&password).unwrap(),
                ..AuthConfig::default()
            },
            store,
            Arc::new(AttemptLimiter::default()),
        )
        .unwrap();
        (login, password)
    }

    fn attempt(password: &str) -> PasswordAttempt<'_> {
        PasswordAttempt {
            client: Some("192.0.2.1".parse().unwrap()),
            origin: "https://meter.example",
            nonce_cookie: Some(NONCE),
            device_cookie: None,
            csrf: NONCE,
            password,
            prior_session: None,
        }
    }

    #[tokio::test]
    async fn csrf_failures_do_not_spend_password_budget_or_issue_sessions() {
        let (login, password) = verifier(SessionStore::new());
        let wrong = random_password();
        for _ in 0..10 {
            let mut request = attempt(&wrong);
            request.origin = "https://attacker.example";
            assert!(matches!(login.attempt(request).await, Err(Reason::CsrfOriginMismatch)));
        }
        let mut request = attempt(&wrong);
        request.nonce_cookie = None;
        assert!(matches!(login.attempt(request).await, Err(Reason::CsrfCookieMissing)));
        let (token, _) = login.attempt(attempt(&password)).await.unwrap();
        assert!(login.sessions.lookup(&token).is_some());
    }

    #[tokio::test]
    async fn capacity_and_address_checks_precede_expensive_verification() {
        let (login, _) = verifier(SessionStore::new());
        let wrong = random_password();
        let _occupied = login.slots.clone().acquire_many_owned(2).await.unwrap();
        for _ in 0..5 {
            let refused = login.attempt(attempt(&wrong)).await;
            assert!(matches!(refused, Err(Reason::VerifierBusy)));
        }
        assert!(matches!(login.attempt(attempt(&wrong)).await, Err(Reason::Throttled)));
        let mut request = attempt(&wrong);
        request.client = None;
        assert!(matches!(login.attempt(request).await, Err(Reason::Throttled)));
    }

    #[tokio::test]
    async fn successful_password_login_rotates_only_the_supplied_session() {
        let store = SessionStore::new();
        let (login, password) = verifier(store.clone());
        let wrong = random_password();
        let (prior, old) = store.create("local-operator", "Local operator", "local", None).unwrap();
        let (_, sibling) = store.create("local-operator", "Local operator", "local", None).unwrap();
        let mut request = attempt(&wrong);
        request.prior_session = Some(&prior);
        assert!(matches!(login.attempt(request).await, Err(Reason::PasswordMismatch)));
        assert!(old.is_active());
        let mut request = attempt(&password);
        request.prior_session = Some(&prior);
        let (_, replacement) = login.attempt(request).await.unwrap();
        assert!(!old.is_active());
        assert!(sibling.is_active() && replacement.is_active());
    }
}

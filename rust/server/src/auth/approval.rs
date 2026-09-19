//! Browser/CLI approval handshakes. HTTP origin, CSRF and rate checks precede these APIs.
use super::{
    grant::{AuthLease, GrantError, MAX_SESSION_GRANTS, secure_browser_origin},
    session::{SessionLease, SessionStore},
};
use base64::{
    Engine as _, alphabet,
    engine::{
        DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig, general_purpose::URL_SAFE_NO_PAD,
    },
};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::time::Instant;

const APPROVAL_LIFETIME: Duration = Duration::from_secs(120);
const MAX_APPROVALS: usize = 256;
const MAX_SESSION_APPROVALS: usize = 8;
const CHALLENGE_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true),
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalKind {
    Cli,
    Browser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalError {
    InvalidChallenge,
    InvalidOrigin,
    NoSession,
    InvalidApproval,
    Capacity,
    GrantCapacity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExchangeError {
    InvalidVerifier,
    InvalidOrigin,
    GrantCapacity,
    RandomUnavailable,
}

/// Public rendering data, never a credential or an authorization decision.
pub struct ApprovalView {
    pub code: String,
    pub browser_origin: Option<String>,
    pub attached: bool,
}

pub enum Exchange {
    Pending,
    Issued { token: String, lease: AuthLease },
}

pub(super) struct Approval {
    session: Option<SessionLease>,
    browser_origin: Option<String>,
    code: String,
    deadline: Instant,
    approved: bool,
}

impl Approval {
    pub(super) fn active_at(&self, now: Instant) -> bool {
        now < self.deadline
            && self
                .session
                .as_ref()
                .is_none_or(|session| session.0.active_at(now))
    }
    fn belongs_to(&self, session: &SessionLease) -> bool {
        self.session
            .as_ref()
            .is_some_and(|parent| std::sync::Arc::ptr_eq(&parent.0, &session.0))
    }
    fn view(&self) -> ApprovalView {
        ApprovalView {
            code: self.code.clone(),
            browser_origin: self.browser_origin.clone(),
            attached: self.session.is_some(),
        }
    }
}

impl SessionStore {
    pub fn begin_cli_approval(
        &self,
        session: &SessionLease,
        challenge: &str,
    ) -> Result<ApprovalView, ApprovalError> {
        let code = verification_code(challenge).ok_or(ApprovalError::InvalidChallenge)?;
        let mut state = self.0.lock().expect("session mutex poisoned");
        let now = Instant::now();
        state.sweep(now);
        if !state.contains(session) || !session.0.active_at(now) {
            return Err(ApprovalError::NoSession);
        }
        if let Some(approval) = state.approvals.get(challenge) {
            return if approval.browser_origin.is_none() && approval.belongs_to(session) {
                Ok(approval.view())
            } else {
                Err(ApprovalError::InvalidApproval)
            };
        }
        if state.approvals.len() >= MAX_APPROVALS
            || state
                .approvals
                .values()
                .filter(|approval| approval.belongs_to(session))
                .count()
                >= MAX_SESSION_APPROVALS
        {
            return Err(ApprovalError::Capacity);
        }
        let approval = Approval {
            session: Some(session.clone()),
            browser_origin: None,
            code,
            deadline: now + APPROVAL_LIFETIME,
            approved: false,
        };
        let view = approval.view();
        state.approvals.insert(challenge.into(), approval);
        Ok(view)
    }

    /// An unauthenticated visit reserves only a bounded challenge/audience pair.
    /// Reentry can attach it to one cookie session, never switch its parent.
    pub fn begin_browser_approval(
        &self,
        challenge: &str,
        origin: &str,
        session: Option<&SessionLease>,
    ) -> Result<ApprovalView, ApprovalError> {
        let code = verification_code(challenge).ok_or(ApprovalError::InvalidChallenge)?;
        if !secure_browser_origin(origin) {
            return Err(ApprovalError::InvalidOrigin);
        }
        let mut state = self.0.lock().expect("session mutex poisoned");
        let now = Instant::now();
        state.sweep(now);
        if let Some(session) = session
            && (!state.contains(session) || !session.0.active_at(now))
        {
            return Err(ApprovalError::NoSession);
        }
        if !state.approvals.contains_key(challenge) {
            if state.approvals.len() >= MAX_APPROVALS {
                return Err(ApprovalError::Capacity);
            }
            state.approvals.insert(
                challenge.into(),
                Approval {
                    session: None,
                    browser_origin: Some(origin.into()),
                    code,
                    deadline: now + APPROVAL_LIFETIME,
                    approved: false,
                },
            );
        }
        let approval = state.approvals.get(challenge).expect("approval exists");
        if approval.browser_origin.as_deref() != Some(origin) {
            return Err(ApprovalError::InvalidApproval);
        }
        if let Some(session) = session {
            if !approval.belongs_to(session) {
                if approval.session.is_some() {
                    return Err(ApprovalError::InvalidApproval);
                }
                if state
                    .approvals
                    .values()
                    .filter(|approval| approval.belongs_to(session))
                    .count()
                    >= MAX_SESSION_APPROVALS
                {
                    return Err(ApprovalError::Capacity);
                }
                state
                    .approvals
                    .get_mut(challenge)
                    .expect("approval exists")
                    .session = Some(session.clone());
            }
            if state.grant_count(session) >= MAX_SESSION_GRANTS {
                return Err(ApprovalError::GrantCapacity);
            }
        }
        Ok(state
            .approvals
            .get(challenge)
            .expect("approval exists")
            .view())
    }

    pub fn approve(
        &self,
        session: &SessionLease,
        challenge: &str,
        kind: ApprovalKind,
    ) -> Result<(), ApprovalError> {
        let mut state = self.0.lock().expect("session mutex poisoned");
        let now = Instant::now();
        let approval = state
            .approvals
            .get(challenge)
            .ok_or(ApprovalError::InvalidApproval)?;
        if !approval.active_at(now)
            || !approval.belongs_to(session)
            || !state.contains(session)
            || approval.browser_origin.is_some() != (kind == ApprovalKind::Browser)
        {
            return Err(ApprovalError::InvalidApproval);
        }
        if kind == ApprovalKind::Browser && state.grant_count(session) >= MAX_SESSION_GRANTS {
            return Err(ApprovalError::GrantCapacity);
        }
        state
            .approvals
            .get_mut(challenge)
            .expect("approval exists")
            .approved = true;
        Ok(())
    }

    /// Preserve a browser challenge through /login -> /auth/cli reentry.
    pub fn browser_approval_redirect(&self, challenge: &str) -> Option<String> {
        let state = self.0.lock().expect("session mutex poisoned");
        let approval = state.approvals.get(challenge)?;
        if !approval.active_at(Instant::now()) {
            return None;
        }
        let origin = approval.browser_origin.as_ref()?;
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("challenge", challenge)
            .append_pair("client_origin", origin)
            .finish();
        Some(format!("/auth/browser?{query}"))
    }

    pub fn exchange_cli(&self, verifier: &str) -> Result<Exchange, ExchangeError> {
        if verifier.len() > 128 {
            return Ok(Exchange::Pending);
        }
        self.exchange_at(verifier, None, Instant::now())
    }

    pub fn exchange_browser(
        &self,
        verifier: &str,
        origin: &str,
    ) -> Result<Exchange, ExchangeError> {
        if !secure_browser_origin(origin) {
            return Err(ExchangeError::InvalidOrigin);
        }
        if !(32..=128).contains(&verifier.len()) {
            return Err(ExchangeError::InvalidVerifier);
        }
        self.exchange_at(verifier, Some(origin), Instant::now())
    }

    fn exchange_at(
        &self,
        verifier: &str,
        origin: Option<&str>,
        now: Instant,
    ) -> Result<Exchange, ExchangeError> {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut state = self.0.lock().expect("session mutex poisoned");
        let Some(approval) = state.approvals.get(&challenge) else {
            return Ok(Exchange::Pending);
        };
        if !approval.active_at(now) {
            state.approvals.remove(&challenge);
            return Ok(Exchange::Pending);
        }
        if approval.browser_origin.as_deref() != origin {
            return Ok(Exchange::Pending);
        }
        let Some(session) = approval.session.clone() else {
            return Ok(Exchange::Pending);
        };
        // Browser capacity is observable even before approval, but only after
        // verifier, audience and attached live parent have matched.
        if origin.is_some() && state.grant_count(&session) >= MAX_SESSION_GRANTS {
            return Err(ExchangeError::GrantCapacity);
        }
        if !approval.approved {
            return Ok(Exchange::Pending);
        }
        match state.issue_grant(&session, origin, now) {
            Ok((token, lease)) => {
                state.approvals.remove(&challenge);
                Ok(Exchange::Issued { token, lease })
            }
            Err(GrantError::NoSession) => Ok(Exchange::Pending),
            Err(GrantError::Capacity) => Err(ExchangeError::GrantCapacity),
            Err(GrantError::RandomUnavailable) => Err(ExchangeError::RandomUnavailable),
            Err(GrantError::InvalidOrigin) => Err(ExchangeError::InvalidOrigin),
        }
    }
}

pub fn valid_challenge(challenge: &str) -> bool {
    challenge_bytes(challenge).is_some()
}

fn challenge_bytes(challenge: &str) -> Option<[u8; 32]> {
    if challenge.len() > 64 {
        return None;
    }
    let normalized: Vec<_> = challenge
        .bytes()
        .filter(|byte| !matches!(byte, b'\r' | b'\n'))
        .collect();
    let mut bytes = [0; 32];
    if CHALLENGE_BASE64.decode_slice(normalized, &mut bytes).ok()? != 32 {
        return None;
    }
    Some(bytes)
}

fn verification_code(challenge: &str) -> Option<String> {
    let bytes = challenge_bytes(challenge)?;
    let value = bytes[..5]
        .iter()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    Some(
        (0..8)
            .rev()
            .map(|position| ALPHABET[((value >> (position * 5)) & 31) as usize] as char)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reentry_keeps_original_deadline_and_revocation_removes_attached_approvals() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "name", "local", None).unwrap();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(b"verifier"));
        store
            .begin_browser_approval(&challenge, "https://client.example", None)
            .unwrap();
        let deadline = store.0.lock().unwrap().approvals[&challenge].deadline;
        store
            .begin_browser_approval(&challenge, "https://client.example", Some(&session))
            .unwrap();
        assert_eq!(
            store.0.lock().unwrap().approvals[&challenge].deadline,
            deadline
        );
        store.revoke(&session);
        assert!(store.0.lock().unwrap().approvals.is_empty());
        assert!(store.browser_approval_redirect(&challenge).is_none());
    }

    #[test]
    fn expired_approval_cannot_be_marked_approved() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "name", "local", None).unwrap();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(b"verifier"));
        store.begin_cli_approval(&session, &challenge).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .approvals
            .get_mut(&challenge)
            .unwrap()
            .deadline = Instant::now();
        assert_eq!(
            store.approve(&session, &challenge, ApprovalKind::Cli),
            Err(ApprovalError::InvalidApproval)
        );
    }

    #[test]
    fn expired_approval_cannot_exchange_and_unknown_verifier_allocates_nothing() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "name", "local", None).unwrap();
        let verifier = "v".repeat(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        store.begin_cli_approval(&session, &challenge).unwrap();
        store
            .approve(&session, &challenge, ApprovalKind::Cli)
            .unwrap();
        let deadline = store.0.lock().unwrap().approvals[&challenge].deadline;
        assert!(matches!(
            store.exchange_at(&verifier, None, deadline).unwrap(),
            Exchange::Pending
        ));
        assert!(matches!(
            store.exchange_cli("unknown").unwrap(),
            Exchange::Pending
        ));
        let state = store.0.lock().unwrap();
        assert!(state.approvals.is_empty());
        assert!(state.grants.is_empty());
    }
}

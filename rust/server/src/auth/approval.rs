//! Browser/CLI approval handshakes. HTTP origin, CSRF and rate checks precede these APIs.
use super::{
    grant::{AuthLease, GrantError, MAX_SESSION_GRANTS},
    session::{SessionLease, SessionStore, State},
};
use crate::sync::lock;
use graphite_meter_core::approval;
use std::{net::IpAddr, time::Duration};
use tokio::time::Instant;

const APPROVAL_LIFETIME: Duration = Duration::from_secs(120);
const MAX_APPROVALS: usize = 256;
const MAX_SESSION_APPROVALS: usize = 8;
const MAX_CLIENT_APPROVALS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalKind {
    Cli,
    Browser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalError {
    NoSession,
    InvalidApproval,
    Capacity,
    GrantCapacity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExchangeError {
    InvalidVerifier,
    GrantCapacity,
    RandomUnavailable,
}

/// Public rendering data, never a credential or an authorization decision.
pub struct ApprovalView {
    pub code: String,
    pub browser_origin: Option<String>,
}

pub enum Exchange {
    Pending,
    Issued { token: String, lease: AuthLease },
}

pub(super) struct Approval {
    client_keys: Vec<String>,
    session: Option<SessionLease>,
    browser_origin: Option<String>,
    code: String,
    deadline: Instant,
    approved: bool,
}

impl Approval {
    pub(super) fn active_at(&self, now: Instant) -> bool {
        now < self.deadline && self.session.as_ref().is_none_or(|session| session.0.active_at(now))
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
        }
    }
}

impl State {
    fn approval_capacity(&self, client: IpAddr, signed_in: bool) -> bool {
        let anonymous = || {
            self.approvals
                .values()
                .filter(|approval| approval.session.is_none())
                .count()
        };
        self.approvals.len() >= MAX_APPROVALS
            || !signed_in && anonymous() >= MAX_APPROVALS / 2
            || crate::client_address::share_full(
                &crate::client_address::client_keys(client),
                MAX_CLIENT_APPROVALS,
                |key| {
                    self.approvals
                        .values()
                        .filter(|approval| approval.client_keys.iter().any(|held| held == key))
                        .count()
                },
            )
    }
}

impl SessionStore {
    pub fn begin_cli_approval(
        &self,
        session: &SessionLease,
        challenge: &str,
        client: IpAddr,
    ) -> Result<ApprovalView, ApprovalError> {
        let mut state = lock(&self.0);
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
        if state.approval_capacity(client, true)
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
            client_keys: crate::client_address::client_keys(client),
            session: Some(session.clone()),
            browser_origin: None,
            code: code(challenge),
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
        client: IpAddr,
    ) -> Result<ApprovalView, ApprovalError> {
        let mut state = lock(&self.0);
        let now = Instant::now();
        state.sweep(now);
        if let Some(session) = session
            && (!state.contains(session) || !session.0.active_at(now))
        {
            return Err(ApprovalError::NoSession);
        }
        if !state.approvals.contains_key(challenge) {
            if state.approval_capacity(client, session.is_some()) {
                return Err(ApprovalError::Capacity);
            }
            state.approvals.insert(
                challenge.into(),
                Approval {
                    client_keys: crate::client_address::client_keys(client),
                    session: None,
                    browser_origin: Some(origin.into()),
                    code: code(challenge),
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
                state.approvals.get_mut(challenge).expect("approval exists").session = Some(session.clone());
            }
            if state.grant_count(session) >= MAX_SESSION_GRANTS {
                return Err(ApprovalError::GrantCapacity);
            }
        }
        Ok(state.approvals.get(challenge).expect("approval exists").view())
    }

    pub fn approve(&self, session: &SessionLease, challenge: &str, kind: ApprovalKind) -> Result<(), ApprovalError> {
        let mut state = lock(&self.0);
        let now = Instant::now();
        let approval = state.approvals.get(challenge).ok_or(ApprovalError::InvalidApproval)?;
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
        state.approvals.get_mut(challenge).expect("approval exists").approved = true;
        Ok(())
    }

    /// Preserve a browser challenge through /login -> /auth/cli reentry.
    pub fn browser_approval_redirect(&self, challenge: &str) -> Option<String> {
        let state = lock(&self.0);
        let approval = state.approvals.get(challenge)?;
        if !approval.active_at(Instant::now()) {
            return None;
        }
        let origin = approval.browser_origin.as_ref()?;
        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("challenge", challenge)
            .append_pair("client_origin", origin)
            .finish();
        Some(format!("{}?{query}", super::AuthRoute::BrowserPage.path()))
    }

    pub fn exchange_cli(&self, verifier: &str) -> Result<Exchange, ExchangeError> {
        if verifier.len() > 128 {
            return Ok(Exchange::Pending);
        }
        self.exchange(verifier, None)
    }

    pub fn exchange_browser(&self, verifier: &str, origin: &str) -> Result<Exchange, ExchangeError> {
        if !(32..=128).contains(&verifier.len()) {
            return Err(ExchangeError::InvalidVerifier);
        }
        self.exchange(verifier, Some(origin))
    }

    fn exchange(&self, verifier: &str, origin: Option<&str>) -> Result<Exchange, ExchangeError> {
        let challenge = approval::challenge(verifier);
        let mut state = lock(&self.0);
        let Some(approval) = state.approvals.get(&challenge) else {
            return Ok(Exchange::Pending);
        };
        if !approval.active_at(Instant::now()) {
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
        match state.issue_grant(&session, origin) {
            Ok((token, lease)) => {
                state.approvals.remove(&challenge);
                Ok(Exchange::Issued { token, lease })
            }
            Err(GrantError::NoSession) => Ok(Exchange::Pending),
            Err(GrantError::Capacity) => Err(ExchangeError::GrantCapacity),
            Err(GrantError::RandomUnavailable) => Err(ExchangeError::RandomUnavailable),
        }
    }
}

pub fn valid_challenge(challenge: &str) -> bool {
    approval::verification_code(challenge).is_some()
}

/// The code a page shows for a challenge the HTTP handler validated.
fn code(challenge: &str) -> String {
    approval::verification_code(challenge).expect("a validated challenge")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_capacity_is_reported_without_revoking_or_approving_existing_clients() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "Name", "local", None).unwrap();
        let verifier = "v".repeat(32);
        let challenge = approval::challenge(&verifier);
        store
            .begin_browser_approval(
                &challenge,
                "https://client.example",
                Some(&session),
                "192.0.2.1".parse().unwrap(),
            )
            .unwrap();
        let grants: Vec<_> = (0..8)
            .map(|_| store.issue_browser_grant(&session, "https://client.example").unwrap().0)
            .collect();
        assert!(matches!(
            store.begin_browser_approval(
                &challenge,
                "https://client.example",
                Some(&session),
                "192.0.2.1".parse().unwrap()
            ),
            Err(ApprovalError::GrantCapacity)
        ));
        assert!(matches!(
            store.approve(&session, &challenge, ApprovalKind::Browser),
            Err(ApprovalError::GrantCapacity)
        ));
        assert!(matches!(
            store.exchange_browser(&verifier, "https://client.example"),
            Err(ExchangeError::GrantCapacity)
        ));
        assert!(matches!(
            store.exchange_browser(&verifier, "https://wrong.example").unwrap(),
            Exchange::Pending
        ));
        for grant in &grants {
            assert!(store.lookup_bearer(grant).is_some());
        }
        store.revoke_grant(&grants[0]);
        assert!(matches!(
            store.exchange_browser(&verifier, "https://client.example").unwrap(),
            Exchange::Pending
        ));
        store.approve(&session, &challenge, ApprovalKind::Browser).unwrap();
        assert!(matches!(
            store.exchange_browser(&verifier, "https://client.example").unwrap(),
            Exchange::Issued { .. }
        ));
    }

    #[test]
    fn reentry_keeps_original_deadline_and_revocation_removes_attached_approvals() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "name", "local", None).unwrap();
        let challenge = approval::challenge("verifier");
        store
            .begin_browser_approval(&challenge, "https://client.example", None, "192.0.2.1".parse().unwrap())
            .unwrap();
        let deadline = store.0.lock().unwrap().approvals[&challenge].deadline;
        store
            .begin_browser_approval(
                &challenge,
                "https://client.example",
                Some(&session),
                "192.0.2.1".parse().unwrap(),
            )
            .unwrap();
        assert_eq!(store.0.lock().unwrap().approvals[&challenge].deadline, deadline);
        store.revoke(&session);
        assert!(store.0.lock().unwrap().approvals.is_empty());
        assert!(store.browser_approval_redirect(&challenge).is_none());
    }

    #[test]
    fn expired_approval_cannot_be_marked_approved() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "name", "local", None).unwrap();
        let challenge = approval::challenge("verifier");
        store
            .begin_cli_approval(&session, &challenge, "192.0.2.1".parse().unwrap())
            .unwrap();
        store.0.lock().unwrap().approvals.get_mut(&challenge).unwrap().deadline = Instant::now();
        assert_eq!(
            store.approve(&session, &challenge, ApprovalKind::Cli),
            Err(ApprovalError::InvalidApproval)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_approval_cannot_exchange_and_unknown_verifier_allocates_nothing() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "name", "local", None).unwrap();
        let verifier = "v".repeat(32);
        let challenge = approval::challenge(&verifier);
        store
            .begin_cli_approval(&session, &challenge, "192.0.2.1".parse().unwrap())
            .unwrap();
        store.approve(&session, &challenge, ApprovalKind::Cli).unwrap();
        tokio::time::advance(APPROVAL_LIFETIME).await;
        assert!(matches!(store.exchange_cli(&verifier).unwrap(), Exchange::Pending));
        assert!(matches!(store.exchange_cli("unknown").unwrap(), Exchange::Pending));
        let state = store.0.lock().unwrap();
        assert!(state.approvals.is_empty());
        assert!(state.grants.is_empty());
    }
}

use super::{
    grant::AuthLease,
    session::{SessionStore, random_token, token_hash},
};
use crate::sync::lock;
use graphite_meter_core::{
    origin::{canonical_origin, target_origin},
    route::{self, Kind},
};
use std::time::{Duration, SystemTime};
use tokio::time::Instant;

const TICKET_LIFETIME: Duration = Duration::from_secs(30);
const MAX_SESSION_TICKETS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicketError {
    NoSession,
    Capacity,
    InvalidTarget,
    RandomUnavailable,
}

/// Raw credential returned only to the minting caller; never stored or logged.
pub struct Ticket {
    pub token: String,
    pub expires: SystemTime,
}

pub(super) struct StoredTicket {
    lease: AuthLease,
    target: String,
    origin: String,
    deadline: Instant,
}

impl StoredTicket {
    pub(super) fn active_at(&self, now: Instant) -> bool {
        now < self.deadline && self.lease.active_at(now)
    }
}

impl SessionStore {
    pub fn mint_ticket(
        &self,
        lease: &AuthLease,
        public_origin: &str,
        target: &str,
        request_origin: &str,
        kind: Kind,
    ) -> Result<Ticket, TicketError> {
        if lease.is_bearer() && lease.browser_origin().is_none() {
            return Err(TicketError::NoSession);
        }
        let (target, target_host, route_kind) = socket_target(target).ok_or(TicketError::InvalidTarget)?;
        let public = target_origin(public_origin)
            .ok()
            .flatten()
            .ok_or(TicketError::InvalidTarget)?;
        if public.scheme != "https" || !target_host.eq_ignore_ascii_case(&public.host) || route_kind != kind {
            return Err(TicketError::InvalidTarget);
        }
        let mut state = lock(&self.0);
        let now = Instant::now();
        state.sweep(now);
        if !state.contains(&lease.session) || !lease.active_at(now) {
            return Err(TicketError::NoSession);
        }
        if state
            .tickets
            .values()
            .filter(|ticket| ticket.lease.session.0.hash == lease.session.0.hash)
            .count()
            >= MAX_SESSION_TICKETS
        {
            return Err(TicketError::Capacity);
        }
        let token = format!(
            "gmw_{}",
            random_token::<32>().map_err(|_| TicketError::RandomUnavailable)?
        );
        let deadline = (now + TICKET_LIFETIME).min(lease.session.0.deadline);
        let expires = (SystemTime::now() + deadline.saturating_duration_since(now)).min(lease.session().expires());
        state.tickets.insert(
            token_hash(&token),
            StoredTicket {
                lease: lease.as_ticket(),
                target,
                origin: request_origin.into(),
                deadline,
            },
        );
        Ok(Ticket { token, expires })
    }

    /// A failed redemption also burns the ticket. The caller supplies HTTPS
    /// authority plus request path without query, and the exact Origin header.
    pub fn consume_ticket(&self, raw: &str, target: &str, origin: &str) -> Option<AuthLease> {
        let mut state = lock(&self.0);
        let ticket = state.tickets.remove(&token_hash(raw))?;
        let (target, _, _) = socket_target(target)?;
        if ticket.target != target || ticket.origin != origin || !ticket.active_at(Instant::now()) {
            return None;
        }
        Some(ticket.lease)
    }
}

fn socket_target(raw: &str) -> Option<(String, String, Kind)> {
    // Split before URL normalization: /other/../wt/ping must not become /wt/ping.
    // Go's mintSocketToken refuses a query or a fragment; a bare '#' leaves url.URL's Fragment empty.
    let raw = raw.strip_suffix('#').unwrap_or(raw);
    if raw.contains(['?', '#', '\\']) {
        return None;
    }
    let (scheme, rest) = raw.split_once("://")?;
    let (authority, path) = rest.split_once('/')?;
    let origin = format!("{scheme}://{authority}");
    let parsed = target_origin(&origin).ok().flatten()?;
    if parsed.scheme != "https" {
        return None;
    }
    let path = unescape(&format!("/{path}"), false)?;
    let route = route::lookup(&path)?;
    Some((
        format!("{}{path}", canonical_origin(&origin).ok()?),
        parsed.host,
        route.kind(),
    ))
}

/// Go's `url.QueryUnescape` for a form value, where '+' is a space, or its `PathUnescape`.
pub(super) fn unescape(raw: &str, form: bool) -> Option<String> {
    let mut out = Vec::with_capacity(raw.len());
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        out.push(match byte {
            b'+' if form => b' ',
            b'%' => {
                let high = (bytes.next()? as char).to_digit(16)?;
                let low = (bytes.next()? as char).to_digit(16)?;
                (high * 16 + low) as u8
            }
            byte => byte,
        });
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::SESSION_LIFETIME;
    const PUBLIC: &str = "https://meter.example";
    const TARGET: &str = "https://meter.example:8443/wt/ping";
    const AUDIENCE: &str = "https://client.example";

    #[tokio::test]
    async fn tickets_follow_browser_grant_revocation_and_refuse_cli_grants() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "Name", "local", None).unwrap();
        let (grant, browser) = store.issue_browser_grant(&session, AUDIENCE).unwrap();
        let (_, sibling) = store.issue_browser_grant(&session, AUDIENCE).unwrap();
        let mint = || store.mint_ticket(&browser, PUBLIC, TARGET, AUDIENCE, Kind::WebTransport);
        let (ticket, unused) = (mint().unwrap(), mint().unwrap());
        let active = store.consume_ticket(&ticket.token, TARGET, AUDIENCE).unwrap();
        assert_eq!(active.owner(), browser.owner());
        assert_ne!(active.owner(), sibling.owner());
        assert_eq!(active.browser_origin(), Some(AUDIENCE));
        store.revoke_grant(&grant);
        tokio::time::timeout(Duration::from_secs(1), active.ended())
            .await
            .unwrap();
        assert!(store.consume_ticket(&unused.token, TARGET, AUDIENCE).is_none());
        assert!(sibling.is_active());
        assert_eq!(mint().err(), Some(TicketError::NoSession));
        let (_, cli) = store.issue_cli_grant(&session).unwrap();
        let refused = store.mint_ticket(&cli, PUBLIC, TARGET, "", Kind::WebTransport);
        assert_eq!(refused.err(), Some(TicketError::NoSession));
    }

    #[tokio::test(start_paused = true)]
    async fn expires_at_thirty_seconds_and_reaps_before_capacity_check() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "name", "local", None).unwrap();
        let lease = AuthLease::cookie(session);
        let mint = || store.mint_ticket(&lease, PUBLIC, TARGET, "", Kind::WebTransport);
        let tickets: Vec<_> = (0..MAX_SESSION_TICKETS).map(|_| mint().unwrap()).collect();
        tokio::time::advance(TICKET_LIFETIME).await;
        assert!(store.consume_ticket(&tickets[0].token, TARGET, "").is_none());
        assert!(mint().is_ok());
        assert_eq!(store.0.lock().unwrap().tickets.len(), 1);
    }

    #[test]
    fn session_removal_eagerly_removes_grants_and_outstanding_tickets() {
        let store = SessionStore::new();
        let (old, session) = store.create("subject", "name", "local", None).unwrap();
        let (_, lease) = store.issue_browser_grant(&session, AUDIENCE).unwrap();
        let target = "https://meter.example/wt/ping";
        assert!(
            store
                .mint_ticket(&lease, PUBLIC, target, AUDIENCE, Kind::WebTransport)
                .is_ok()
        );
        store.create("subject", "name", "local", Some(&old)).unwrap();
        let state = store.0.lock().unwrap();
        assert!(state.grants.is_empty());
        assert!(state.tickets.is_empty());
        assert!(!lease.is_active());
    }

    #[tokio::test(start_paused = true)]
    async fn parent_deadline_limits_ticket_and_grant_lifetimes() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "name", "local", None).unwrap();
        let (grant, lease) = store.issue_browser_grant(&session, AUDIENCE).unwrap();
        let left = Duration::from_secs(5);
        tokio::time::advance(SESSION_LIFETIME - left).await;
        let ticket = store.mint_ticket(&lease, PUBLIC, TARGET, AUDIENCE, Kind::WebTransport);
        let ticket = ticket.unwrap();
        // The paused clock leaves wall time behind, so the ticket's expiry follows the parent's time left.
        assert!(ticket.expires <= SystemTime::now() + left);
        tokio::time::advance(left).await;
        assert!(store.consume_ticket(&ticket.token, TARGET, AUDIENCE).is_none());
        store.0.lock().unwrap().sweep(Instant::now());
        assert!(store.lookup_bearer(&grant).is_none());
    }
}

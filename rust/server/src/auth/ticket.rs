use super::{
    grant::AuthLease,
    session::{SessionStore, random_token, token_hash},
};
use graphite_meter_core::{
    origin::{canonical_origin, target_origin},
    route::{self, Kind},
};
use std::time::{Duration, SystemTime};
use tokio::time::Instant;

const TICKET_LIFETIME: Duration = Duration::from_secs(30);
const MAX_SESSION_TICKETS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketKind {
    WebSocket,
    WebTransport,
}

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
        kind: SocketKind,
    ) -> Result<Ticket, TicketError> {
        self.mint_ticket_at(
            lease,
            public_origin,
            target,
            request_origin,
            kind,
            Instant::now(),
        )
    }

    fn mint_ticket_at(
        &self,
        lease: &AuthLease,
        public_origin: &str,
        target: &str,
        request_origin: &str,
        kind: SocketKind,
        now: Instant,
    ) -> Result<Ticket, TicketError> {
        if lease.is_bearer() && lease.browser_origin().is_none() {
            return Err(TicketError::NoSession);
        }
        let (target, target_host, route_kind) =
            socket_target(target).ok_or(TicketError::InvalidTarget)?;
        let public = target_origin(public_origin)
            .ok()
            .flatten()
            .ok_or(TicketError::InvalidTarget)?;
        let expected = match kind {
            SocketKind::WebSocket => Kind::WebSocket,
            SocketKind::WebTransport => Kind::WebTransport,
        };
        if public.scheme != "https"
            || !target_host.eq_ignore_ascii_case(&public.host)
            || route_kind != expected
        {
            return Err(TicketError::InvalidTarget);
        }
        let mut state = self.0.lock().expect("session mutex poisoned");
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
        let expires = (SystemTime::now() + deadline.saturating_duration_since(now))
            .min(lease.session().expires());
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
        self.consume_ticket_at(raw, target, origin, Instant::now())
    }

    fn consume_ticket_at(
        &self,
        raw: &str,
        target: &str,
        origin: &str,
        now: Instant,
    ) -> Option<AuthLease> {
        let mut state = self.0.lock().expect("session mutex poisoned");
        let ticket = state.tickets.remove(&token_hash(raw))?;
        let (target, _, _) = socket_target(target)?;
        if ticket.target != target || ticket.origin != origin || !ticket.active_at(now) {
            return None;
        }
        Some(ticket.lease)
    }
}

fn socket_target(raw: &str) -> Option<(String, String, Kind)> {
    // Split before URL normalization: /other/../wt/ping must not become /wt/ping.
    // Go's URL parser permits an empty fragment but rejects nonempty fragments.
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
    let path = decode_path(&format!("/{path}"))?;
    let route = route::lookup(&path)?;
    Some((
        format!("{}{path}", canonical_origin(&origin).ok()?),
        parsed.host,
        route.kind(),
    ))
}

fn decode_path(raw: &str) -> Option<String> {
    let mut path = Vec::with_capacity(raw.len());
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        path.push(if byte == b'%' {
            let high = (bytes.next()? as char).to_digit(16)?;
            let low = (bytes.next()? as char).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    String::from_utf8(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::SESSION_LIFETIME;

    #[test]
    fn expires_at_thirty_seconds_and_reaps_before_capacity_check() {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "name", "local", None).unwrap();
        let lease = AuthLease::cookie(session);
        let now = Instant::now();
        let target = "https://meter.example/wt/ping";
        let mut tokens = Vec::new();
        for _ in 0..8 {
            tokens.push(
                store
                    .mint_ticket_at(
                        &lease,
                        "https://meter.example",
                        target,
                        "",
                        SocketKind::WebTransport,
                        now,
                    )
                    .unwrap(),
            );
        }
        assert!(
            store
                .consume_ticket_at(&tokens[0].token, target, "", now + TICKET_LIFETIME)
                .is_none()
        );
        assert!(
            store
                .mint_ticket_at(
                    &lease,
                    "https://meter.example",
                    target,
                    "",
                    SocketKind::WebTransport,
                    now + TICKET_LIFETIME
                )
                .is_ok()
        );
        assert_eq!(store.0.lock().unwrap().tickets.len(), 1);
    }

    #[test]
    fn session_removal_eagerly_removes_grants_and_outstanding_tickets() {
        let store = SessionStore::new();
        let (old, session) = store.create("subject", "name", "local", None).unwrap();
        let (_, lease) = store
            .issue_browser_grant(&session, "https://client.example")
            .unwrap();
        store
            .mint_ticket(
                &lease,
                "https://meter.example",
                "https://meter.example/wt/ping",
                "https://client.example",
                SocketKind::WebTransport,
            )
            .unwrap();
        store
            .create("subject", "name", "local", Some(&old))
            .unwrap();
        let state = store.0.lock().unwrap();
        assert!(state.grants.is_empty());
        assert!(state.tickets.is_empty());
        assert!(!lease.is_active());
    }

    #[test]
    fn parent_deadline_limits_ticket_and_grant_lifetimes() {
        let store = SessionStore::new();
        let now = Instant::now();
        let (_, session) = store
            .create_at(
                "subject",
                "name",
                "local",
                None,
                SystemTime::now() - SESSION_LIFETIME + Duration::from_secs(5),
                now - SESSION_LIFETIME + Duration::from_secs(5),
            )
            .unwrap();
        let (grant, lease) = store
            .issue_browser_grant(&session, "https://client.example")
            .unwrap();
        let target = "https://meter.example/wt/ping";
        let ticket = store
            .mint_ticket_at(
                &lease,
                "https://meter.example",
                target,
                "https://client.example",
                SocketKind::WebTransport,
                now,
            )
            .unwrap();
        assert_eq!(ticket.expires, session.session().expires());
        assert!(
            store
                .consume_ticket_at(
                    &ticket.token,
                    target,
                    "https://client.example",
                    now + Duration::from_secs(5)
                )
                .is_none()
        );
        store.0.lock().unwrap().sweep(now + Duration::from_secs(5));
        assert!(store.lookup_bearer(&grant).is_none());
    }
}

//! Bounds what the server's work may hold, through the buffer budget and the quotas keyed by client.

pub mod budget;
pub mod quota;

pub use budget::{Budget, Lease, Pressure, Usage};
pub use quota::{Hold, Quota, QuotaUsage, Refusal};

use crate::{
    config::Limits,
    peer::{ClientKey, ClientKeys},
};
use graphite_meter_net::quic::{RECEIVE_WINDOW, RECEIVE_WINDOW_FLOOR};

/// One client address may hold this many QUIC connections within its connection share.
pub const QUIC_PER_CLIENT: usize = 8;
/// A funded QUIC connection holds this much receive credit past its floor.
pub const CONNECTION_CREDIT: usize = (RECEIVE_WINDOW - RECEIVE_WINDOW_FLOOR) as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Tcp,
    Quic,
}

/// Every listener shares these quotas.
pub struct Quotas {
    limits: Limits,
    connections: Quota,
    quic: Quota,
    operations: Quota,
    sessions: Quota,
    credit: Quota,
}

impl Quotas {
    /// `operator` is the principal every password login shares, which bounds no window credit.
    pub fn new(limits: Limits, budget: &Budget, operator: Option<ClientKey>) -> Self {
        let quic_share = limits.connections_per_client.get().min(QUIC_PER_CLIENT);
        Self {
            limits,
            connections: Quota::new(limits.connections.get(), limits.connections_per_client.get()),
            quic: Quota::new(usize::MAX, quic_share),
            operations: Quota::new(limits.operations.get(), limits.operations_per_client.get()),
            sessions: Quota::new(limits.sessions.get(), limits.sessions_per_client.get()),
            credit: Quota::client_windows(budget.clone(), quic_share * CONNECTION_CREDIT, operator),
        }
    }

    /// Admits a connection keyed by its socket address. A QUIC connection also takes one of the client's QUIC shares.
    pub fn connection(&self, keys: &ClientKeys, transport: Transport) -> Option<Hold> {
        let hold = self.connections.acquire(keys, 1).ok()?;
        match transport {
            Transport::Tcp => Some(hold),
            Transport::Quic => Some(hold.join(self.quic.acquire(keys, 1).ok()?)),
        }
    }

    /// Tells whether any key of a source holds a QUIC connection, so that its unvalidated handshakes need Retry.
    pub fn holds_quic(&self, keys: &ClientKeys) -> bool {
        self.quic.holds_any(keys)
    }

    /// Tells whether a quarter of the connection capacity is used, so that unvalidated QUIC handshakes need Retry.
    pub fn connections_crowded(&self) -> bool {
        self.connections.usage().active >= self.limits.connections.get() / 4
    }

    /// Takes a measurement handler.
    pub fn operation(&self, keys: &ClientKeys) -> Result<Hold, Refusal> {
        self.operations.acquire(keys, 1)
    }

    /// Admits a WebTransport transfer session, which also takes a handler from the shared pool.
    pub fn session(&self, keys: &ClientKeys) -> Result<Hold, Refusal> {
        self.sessions.acquire_within(keys, 1, Some(&self.operations))
    }

    /// Credits the receive window of a connection an admitted client funds, none past its share or the clients' half.
    pub fn credit(&self, keys: &ClientKeys, bytes: usize) -> Option<Hold> {
        self.credit.acquire(keys, bytes).ok()
    }

    /// Reports active handlers and their limit, as `/probe` reports load.
    pub fn load(&self) -> (usize, usize) {
        (self.operations.usage().active, self.limits.operations.get())
    }

    /// Counts the open connections.
    pub fn connections(&self) -> usize {
        self.connections.usage().active
    }

    /// Completes once the last connection closes, also when that happened since the last wait ended.
    pub async fn idle(&self) {
        self.connections.idle().await;
    }

    /// Writes the verbose admission line, with handlers, sessions and connections, their peaks and refusals.
    pub fn admission(&self) -> String {
        let (handlers, sessions) = (self.operations.usage(), self.sessions.usage());
        let connections = self.connections.usage();
        format!(
            "handlers {} active / {} peak, rejected {} pool + {} client; sessions {} active / {} max, \
             {} per client, rejected {} budget + {} client; connections {} active / {} peak, rejected {} global + {} \
             client",
            handlers.active,
            handlers.peak,
            handlers.refused_total,
            handlers.refused_client,
            sessions.active,
            self.limits.sessions,
            self.limits.sessions_per_client,
            sessions.refused_total,
            sessions.refused_client,
            connections.active,
            connections.peak,
            connections.refused_total,
            connections.refused_client,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Holder, Principal};

    fn limits() -> Limits {
        let count = |count| std::num::NonZero::new(count).unwrap();
        Limits {
            operations: count(3),
            operations_per_client: count(2),
            sessions: count(2),
            sessions_per_client: count(1),
            connections: count(64),
            connections_per_client: count(32),
        }
    }

    fn client(address: &str) -> ClientKeys {
        ClientKeys::address(address.parse().unwrap())
    }

    #[test]
    fn a_client_holds_eight_quic_connections_within_its_connection_share() {
        let quotas = Quotas::new(limits(), &Budget::new(usize::MAX), None);
        let source = client("192.0.2.1");
        let quic: Vec<_> = (0..8)
            .map(|_| quotas.connection(&source, Transport::Quic).unwrap())
            .collect();
        assert!(quotas.connection(&source, Transport::Quic).is_none());
        assert!(quotas.holds_quic(&source));
        let tcp: Vec<_> = (0..24)
            .map(|_| quotas.connection(&source, Transport::Tcp).unwrap())
            .collect();
        assert!(quotas.connection(&source, Transport::Tcp).is_none(), "32 connections in all");
        drop((quic, tcp));
        assert!(!quotas.holds_quic(&source));
        let proxy = ClientKeys::Exempt;
        let proxied: Vec<_> = (0..40)
            .map(|_| quotas.connection(&proxy, Transport::Quic).unwrap())
            .collect();
        assert_eq!(proxied.len(), 40, "a trusted proxy charges only the total");
    }

    #[test]
    fn window_credit_covers_eight_connection_windows_per_client() {
        let quotas = Quotas::new(limits(), &Budget::new(usize::MAX), Some(ClientKey::Principal(Principal::operator())));
        let source = client("2001:db8::1");
        let funded: Vec<_> = (0..8)
            .map(|_| quotas.credit(&source, CONNECTION_CREDIT).unwrap())
            .collect();
        assert!(quotas.credit(&source, 1).is_none());
        let operator = |id: &str| ClientKeys::Auth(Holder::Login(id.into()), Principal::operator());
        let logins: Vec<_> = ["a", "b", "c"]
            .map(|id| quotas.credit(&operator(id), 8 * CONNECTION_CREDIT))
            .into();
        assert!(logins.iter().all(Option::is_some), "password logins are bounded one by one");
        drop(funded);
        assert!(quotas.credit(&source, 8 * CONNECTION_CREDIT).is_some());
    }
}

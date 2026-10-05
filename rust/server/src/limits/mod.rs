//! What the server's work may hold: the buffer budget and the quotas keyed by client.

pub mod budget;
pub mod quota;

pub use budget::{Budget, Lease, Pressure, Usage};
pub use quota::{Hold, Quota, QuotaUsage, Refusal};

use crate::{
    config::Limits,
    peer::{ClientKey, ClientKeys},
};
use graphite_meter_net::quic::{RECEIVE_WINDOW, RECEIVE_WINDOW_FLOOR};

/// The QUIC connections one client address may hold within its connection share.
pub const QUIC_PER_CLIENT: usize = 8;
/// The receive credit a funded QUIC connection holds past its floor.
pub const CONNECTION_CREDIT: usize = (RECEIVE_WINDOW - RECEIVE_WINDOW_FLOOR) as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Tcp,
    Quic,
}

/// The quotas every listener shares.
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
        let quic_share = limits.connections_per_client.min(QUIC_PER_CLIENT);
        Self {
            limits,
            connections: Quota::new(limits.connections, limits.connections_per_client),
            quic: Quota::new(usize::MAX, quic_share),
            operations: Quota::new(limits.operations, limits.operations_per_client),
            sessions: Quota::new(limits.sessions, limits.sessions_per_client),
            credit: Quota::client_windows(budget.clone(), quic_share * CONNECTION_CREDIT, operator),
        }
    }

    /// A connection keyed by its socket address; a QUIC connection also takes one of the client's QUIC shares.
    pub fn connection(&self, keys: &ClientKeys, transport: Transport) -> Option<Hold> {
        let hold = self.connections.acquire(keys, 1).ok()?;
        match transport {
            Transport::Tcp => Some(hold),
            Transport::Quic => Some(hold.join(self.quic.acquire(keys, 1).ok()?)),
        }
    }

    /// Whether any key of a source holds a QUIC connection, so that its unvalidated handshakes need Retry.
    pub fn holds_quic(&self, keys: &ClientKeys) -> bool {
        self.quic.holds_any(keys)
    }

    /// Whether a quarter of the connection capacity is used, so that unvalidated QUIC handshakes need Retry.
    pub fn connections_crowded(&self) -> bool {
        self.connections.usage().active >= self.limits.connections / 4
    }

    /// A measurement handler.
    pub fn operation(&self, keys: &ClientKeys) -> Result<Hold, Refusal> {
        self.operations.acquire(keys, 1)
    }

    /// A WebTransport transfer session, which also takes a handler from the shared pool.
    pub fn session(&self, keys: &ClientKeys) -> Result<Hold, Refusal> {
        self.sessions.acquire_within(keys, 1, &self.operations)
    }

    /// Receive-window credit for a connection an admitted client funds; none past its share or the clients' half.
    pub fn credit(&self, keys: &ClientKeys, bytes: usize) -> Option<Hold> {
        self.credit.acquire(keys, bytes).ok()
    }

    /// Active handlers and their limit, as `/probe` reports load.
    pub fn load(&self) -> (usize, usize) {
        (self.operations.usage().active, self.limits.operations)
    }

    /// The open connections.
    pub fn connections(&self) -> usize {
        self.connections.usage().active
    }

    /// Completes once the last connection closes, also when that happened since the last wait ended.
    pub async fn idle(&self) {
        self.connections.idle().await;
    }

    /// The verbose `[gm:admission]` line: handlers, sessions and connections with their peaks and refusals.
    pub fn admission(&self) -> String {
        let (handlers, sessions) = (self.operations.usage(), self.sessions.usage());
        let connections = self.connections.usage();
        format!(
            "[gm:admission] handlers {} active / {} peak, rejected {} pool + {} client; sessions {} active / {} max, \
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
    use crate::auth::Holder;

    fn limits() -> Limits {
        Limits {
            operations: 3,
            operations_per_client: 2,
            sessions: 2,
            sessions_per_client: 1,
            connections: 64,
            connections_per_client: 32,
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
    fn sessions_take_handlers_from_the_shared_pool() {
        let quotas = Quotas::new(limits(), &Budget::new(usize::MAX), None);
        let (a, b) = (client("192.0.2.1"), client("192.0.2.2"));
        let session = quotas.session(&a).unwrap();
        assert_eq!(quotas.session(&a).err(), Some(Refusal::Client));
        let operations = [quotas.operation(&a).unwrap(), quotas.operation(&a).unwrap()];
        assert_eq!(
            quotas.load(),
            (3, 3),
            "a session's handler counts toward the pool, not the client's share"
        );
        assert_eq!(quotas.operation(&b).err(), Some(Refusal::Total));
        assert_eq!(quotas.session(&b).err(), Some(Refusal::Total));
        drop((session, operations));
        assert_eq!(quotas.load(), (0, 3));
    }

    #[test]
    fn the_admission_line_counts_peaks_and_refusals_as_go_does() {
        let quotas = Quotas::new(limits(), &Budget::new(usize::MAX), None);
        let (a, b) = (client("192.0.2.1"), client("192.0.2.2"));
        let session = quotas.session(&a).unwrap();
        assert_eq!(quotas.session(&a).err(), Some(Refusal::Client));
        let operations = [quotas.operation(&b).unwrap(), quotas.operation(&b).unwrap()];
        assert_eq!(quotas.operation(&b).err(), Some(Refusal::Client));
        assert_eq!(quotas.operation(&a).err(), Some(Refusal::Total));
        assert_eq!(quotas.session(&b).err(), Some(Refusal::Total), "the handler pool is full");
        drop(operations);
        let connection = quotas.connection(&a, Transport::Tcp).unwrap();
        drop(session);
        assert_eq!(
            quotas.admission(),
            "[gm:admission] handlers 0 active / 3 peak, rejected 2 pool + 1 client; sessions 0 active / 2 max, 1 per \
             client, rejected 0 budget + 1 client; connections 1 active / 1 peak, rejected 0 global + 0 client"
        );
        assert_eq!(quotas.connections(), 1);
        drop(connection);
        assert_eq!(quotas.connections(), 0);
    }

    #[test]
    fn window_credit_covers_eight_connection_windows_per_client() {
        let quotas = Quotas::new(limits(), &Budget::new(usize::MAX), Some(ClientKey::Principal("op".into())));
        let source = client("2001:db8::1");
        let funded: Vec<_> = (0..8)
            .map(|_| quotas.credit(&source, CONNECTION_CREDIT).unwrap())
            .collect();
        assert!(quotas.credit(&source, 1).is_none());
        let operator = |id: &str| ClientKeys::Auth(Holder::Login(id.into()), "op".into());
        let logins: Vec<_> = ["a", "b", "c"]
            .map(|id| quotas.credit(&operator(id), 8 * CONNECTION_CREDIT))
            .into();
        assert!(logins.iter().all(Option::is_some), "password logins are bounded one by one");
        drop(funded);
        assert!(quotas.credit(&source, 8 * CONNECTION_CREDIT).is_some());
    }
}

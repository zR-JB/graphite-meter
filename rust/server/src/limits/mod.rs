//! What the server's work may hold: the buffer budget and the quotas keyed by client.

mod budget;
mod quota;

pub use budget::{Budget, Lease, Pressure, Usage};
pub use quota::{Hold, Quota, QuotaUsage, Refusal};

use crate::{
    config::Limits,
    log,
    log::{Level, PerPeer, RateLimited},
    peer::{ClientKey, ClientKeys},
};
use graphite_meter_net::quic::{RECEIVE_WINDOW, RECEIVE_WINDOW_FLOOR};
use std::net::IpAddr;

/// A client's admitted uploads hold receive credit for this many QUIC connection windows at once.
pub const WINDOWS_PER_CLIENT: usize = 8;
/// The receive credit a funded QUIC connection holds past its floor.
pub const CONNECTION_CREDIT: usize = (RECEIVE_WINDOW - RECEIVE_WINDOW_FLOOR) as usize;

/// A refused connection is closed at accept, before any request could carry a status, so only the log tells.
static CLIENT_REFUSALS: PerPeer<ClientKey> = PerPeer::new();
static TOTAL_REFUSALS: RateLimited = RateLimited::new(
    Level::Warn,
    "admission",
    "refused connections",
    module_path!(),
);

pub struct Quotas {
    limits: Limits,
    connections: Quota,
    /// Unbounded: it only tells whether a source has a QUIC handshake in progress.
    handshakes: Quota,
    operations: Quota,
    sessions: Quota,
    credit: Quota,
}

impl Quotas {
    /// `operator` is the principal every password login shares, which bounds no window credit.
    pub fn new(limits: Limits, budget: &Budget, operator: Option<ClientKey>) -> Self {
        Self {
            limits,
            connections: Quota::new(
                limits.connections.get(),
                limits.connections_per_client.get(),
            ),
            handshakes: Quota::new(usize::MAX, usize::MAX),
            operations: Quota::new(limits.operations.get(), limits.operations_per_client.get()),
            sessions: Quota::new(limits.sessions.get(), limits.sessions_per_client.get()),
            credit: Quota::client_windows(
                budget.clone(),
                WINDOWS_PER_CLIENT * CONNECTION_CREDIT,
                operator,
            ),
        }
    }

    /// A connection from `peer` keyed by `keys`, TCP or QUIC alike.
    pub fn connection(&self, peer: IpAddr, keys: &ClientKeys) -> Option<Hold> {
        match self.connections.acquire(keys, 1) {
            Ok(hold) => return Some(hold),
            Err(Refusal::Client) => {}
            Err(Refusal::Total) => {
                TOTAL_REFUSALS.write(format_args!(
                    "connection from {peer} refused: GM_MAX_CONNECTIONS={} reached; raise it within \
                     GM_MAX_BUFFER_BYTES",
                    self.limits.connections
                ));
                return None;
            }
        }
        let held = CLIENT_REFUSALS.admit(keys.narrowest()?)?;
        let (per_client, refused) = (self.limits.connections_per_client, held + 1);
        log!(
            Warn,
            "admission",
            "connections from {peer} refused: GM_MAX_CONNECTIONS_PER_CLIENT={per_client} reached, {refused} since \
             its last line; set GM_TRUSTED_PROXIES for a proxy, or raise the limit for clients sharing an address"
        );
        None
    }

    /// Held while a QUIC handshake from `keys` is in progress, so that the source's next unvalidated one needs Retry.
    pub fn handshake(&self, keys: &ClientKeys) -> Option<Hold> {
        self.handshakes.acquire(keys, 1).ok()
    }

    pub fn handshaking(&self, keys: &ClientKeys) -> bool {
        self.handshakes.holds_any(keys)
    }

    /// Whether a quarter of the connection capacity is used, so that unvalidated QUIC handshakes need Retry.
    pub fn connections_crowded(&self) -> bool {
        self.connections.usage().active >= self.limits.connections.get() / 4
    }

    pub fn operation(&self, keys: &ClientKeys) -> Result<Hold, Refusal> {
        self.operations.acquire(keys, 1)
    }

    /// A WebTransport transfer session, which also takes a handler from the shared pool.
    pub fn session(&self, keys: &ClientKeys) -> Result<Hold, Refusal> {
        self.sessions
            .acquire_within(keys, 1, Some(&self.operations))
    }

    /// Receive-window credit for a connection an admitted client funds; none past its share or the clients' half.
    pub fn credit(&self, keys: &ClientKeys, bytes: usize) -> Option<Hold> {
        self.credit.acquire(keys, bytes).ok()
    }

    /// Active handlers and their limit, as `/probe` reports load.
    pub fn load(&self) -> (usize, usize) {
        (self.operations.usage().active, self.limits.operations.get())
    }

    pub fn connections(&self) -> usize {
        self.connections.usage().active
    }

    /// Completes once the last connection closes, also when that happened since the last wait ended.
    pub async fn idle(&self) {
        self.connections.idle().await;
    }

    /// The verbose admission line: handlers, sessions and connections with their peaks and refusals.
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
    fn a_client_holds_connections_of_any_transport_within_its_share() {
        let quotas = Quotas::new(limits(), &Budget::new(usize::MAX), None);
        let (ip, source) = ("192.0.2.1".parse().unwrap(), client("192.0.2.1"));
        let held: Vec<_> = (0..32)
            .map(|_| quotas.connection(ip, &source).unwrap())
            .collect();
        assert!(quotas.connection(ip, &source).is_none());
        drop(held);
        let proxy = ClientKeys::Exempt;
        let proxied: Vec<_> = (0..40)
            .map(|_| quotas.connection(ip, &proxy).unwrap())
            .collect();
        assert_eq!(proxied.len(), 40, "a trusted proxy charges only the total");
    }

    #[test]
    fn a_source_is_handshaking_while_it_holds_a_handshake() {
        let quotas = Quotas::new(limits(), &Budget::new(usize::MAX), None);
        let source = client("192.0.2.1");
        assert!(!quotas.handshaking(&source));
        let handshakes: Vec<_> = (0..100)
            .map(|_| quotas.handshake(&source).unwrap())
            .collect();
        assert!(quotas.handshaking(&source));
        assert!(!quotas.handshaking(&client("192.0.2.2")));
        drop(handshakes);
        assert!(!quotas.handshaking(&source));
        let _proxied = quotas.handshake(&ClientKeys::Exempt).unwrap();
        assert!(
            !quotas.handshaking(&ClientKeys::Exempt),
            "a trusted proxy's clients are not one source"
        );
    }

    #[test]
    fn window_credit_covers_eight_connection_windows_per_client() {
        let quotas = Quotas::new(
            limits(),
            &Budget::new(usize::MAX),
            Some(ClientKey::Principal(Principal::operator())),
        );
        let source = client("2001:db8::1");
        let funded: Vec<_> = (0..8)
            .map(|_| quotas.credit(&source, CONNECTION_CREDIT).unwrap())
            .collect();
        assert!(quotas.credit(&source, 1).is_none());
        let operator = |id: &str| ClientKeys::Auth(Holder::Login(id.into()), Principal::operator());
        let logins: Vec<_> = ["a", "b", "c"]
            .map(|id| quotas.credit(&operator(id), 8 * CONNECTION_CREDIT))
            .into();
        assert!(
            logins.iter().all(Option::is_some),
            "password logins are bounded one by one"
        );
        drop(funded);
        assert!(quotas.credit(&source, 8 * CONNECTION_CREDIT).is_some());
    }
}

//! Connection capacity is acquired before protocol setup and owned until teardown.
use crate::{client_address::Shares, sync::lock};
use ipnet::IpNet;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

/// Go's per-client QUIC share: the QUIC connections one client may hold, past the per-client limit.
pub(crate) const QUIC_PER_CLIENT: usize = 8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub active: usize,
    pub peak: usize,
    pub rejected_global: u64,
    pub rejected_client: u64,
}

#[derive(Default)]
struct Counts {
    stats: Stats,
    clients: Shares,
    quic: Shares,
}
struct Inner {
    global_max: usize,
    client_max: usize,
    trusted: Vec<IpNet>,
    counts: Mutex<Counts>,
}

#[derive(Clone)]
pub struct Connections(Arc<Inner>);

#[must_use]
pub struct Permit {
    owner: Connections,
    keys: Vec<String>,
    quic: bool,
}

impl Connections {
    pub fn new(global_max: usize, client_max: usize, trusted: Vec<IpNet>) -> Self {
        Self(Arc::new(Inner {
            global_max,
            client_max,
            trusted,
            counts: Mutex::default(),
        }))
    }

    /// Trusted proxies are exempt, as in Go.
    fn keys(&self, peer: SocketAddr) -> Vec<String> {
        let addr = peer.ip().to_canonical();
        if self.0.trusted.iter().any(|prefix| prefix.contains(&addr)) {
            Vec::new()
        } else {
            crate::client_address::client_keys(addr)
        }
    }

    /// Whether any of the peer's keys holds a QUIC connection: Go then has an unvalidated source answer Retry,
    /// so spoofed Initials cannot fill a victim's QUIC share.
    pub fn holds_quic(&self, peer: SocketAddr) -> bool {
        let keys = self.keys(peer);
        lock(&self.0.counts).quic.holds_any(&keys)
    }

    /// A QUIC connection also takes Go's per-client QUIC share. The stats count which limit refused one.
    pub fn acquire(&self, peer: SocketAddr, quic: bool) -> Option<Permit> {
        let keys = self.keys(peer);
        let mut counts = lock(&self.0.counts);
        if counts.clients.full(&keys, self.0.client_max) {
            counts.stats.rejected_client = counts.stats.rejected_client.saturating_add(1);
            return None;
        }
        if counts.stats.active >= self.0.global_max {
            counts.stats.rejected_global = counts.stats.rejected_global.saturating_add(1);
            return None;
        }
        // Checked last and left out of the counters, like Go's QUIC share.
        if quic && counts.quic.full(&keys, self.0.client_max.min(QUIC_PER_CLIENT)) {
            return None;
        }
        counts.stats.active += 1;
        counts.stats.peak = counts.stats.peak.max(counts.stats.active);
        counts.clients.hold(&keys);
        if quic {
            counts.quic.hold(&keys);
        }
        Some(Permit {
            owner: self.clone(),
            keys,
            quic,
        })
    }

    pub fn stats(&self) -> Stats {
        lock(&self.0.counts).stats
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut counts = lock(&self.owner.0.counts);
        counts.stats.active -= 1;
        counts.clients.release(&self.keys);
        if self.quic {
            counts.quic.release(&self.keys);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_quic_connections_take_the_per_client_quic_share() {
        let connections = super::Connections::new(64, 64, vec![]);
        let peer = "192.0.2.1:1".parse().unwrap();
        let quic: Vec<_> = (0..8).map(|_| connections.acquire(peer, true).unwrap()).collect();
        assert!(connections.acquire(peer, true).is_none());
        // Like Go, TCP connections from the same address count only toward the client's total.
        let tcp: Vec<_> = (0..16).map(|_| connections.acquire(peer, false).unwrap()).collect();
        assert_eq!(connections.stats().active, quic.len() + tcp.len());
    }

    #[test]
    fn a_quic_connection_marks_every_prefix_of_its_source_but_never_a_trusted_proxy() {
        let connections = super::Connections::new(64, 64, vec!["192.0.2.0/24".parse().unwrap()]);
        let peer = "[2001:db8:1:2::1]:1".parse().unwrap();
        let _tcp = connections.acquire(peer, false).unwrap();
        assert!(!connections.holds_quic(peer), "a TCP connection holds no QUIC share");
        let quic = connections.acquire(peer, true).unwrap();
        // As in Go, any of the /64, /56 and /48 keys holding a QUIC connection counts.
        for sibling in ["[2001:db8:1:2::2]:1", "[2001:db8:1:ff::1]:1", "[2001:db8:1:ff00::1]:1"] {
            assert!(connections.holds_quic(sibling.parse().unwrap()), "{sibling}");
        }
        assert!(!connections.holds_quic("[2001:db8:2::1]:1".parse().unwrap()));
        drop(quic);
        assert!(!connections.holds_quic(peer));
        let proxy = "192.0.2.1:1".parse().unwrap();
        let _proxied = connections.acquire(proxy, true).unwrap();
        assert!(!connections.holds_quic(proxy));
    }
}

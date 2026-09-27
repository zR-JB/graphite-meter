//! Connection capacity is acquired before protocol setup and owned until teardown.
use ipnet::IpNet;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub active: usize,
    pub peak: usize,
    pub rejected_global: u64,
    pub rejected_client: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    GlobalFull,
    ClientFull,
}

#[derive(Default)]
struct Counts {
    stats: Stats,
    clients: HashMap<String, usize>,
    buffered: HashMap<String, usize>,
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
    buffered: bool,
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

    pub fn acquire(&self, peer: SocketAddr) -> Result<Permit, Refusal> {
        self.acquire_buffered(peer, false)
    }

    pub fn acquire_buffered(&self, peer: SocketAddr, buffered: bool) -> Result<Permit, Refusal> {
        let addr = peer.ip().to_canonical();
        let keys = if self.0.trusted.iter().any(|prefix| prefix.contains(&addr)) {
            Vec::new()
        } else {
            crate::client_address::client_keys(addr)
        };
        let mut counts = crate::admission::recover(&self.0.counts, "connection");
        if crate::client_address::share_full(&keys, self.0.client_max, |key| {
            counts.clients.get(key).copied().unwrap_or_default()
        }) || buffered
            && crate::client_address::share_full(&keys, self.0.client_max.min(8), |key| {
                counts.buffered.get(key).copied().unwrap_or_default()
            })
        {
            counts.stats.rejected_client = counts.stats.rejected_client.saturating_add(1);
            return Err(Refusal::ClientFull);
        }
        if counts.stats.active >= self.0.global_max {
            counts.stats.rejected_global = counts.stats.rejected_global.saturating_add(1);
            return Err(Refusal::GlobalFull);
        }
        counts.stats.active += 1;
        counts.stats.peak = counts.stats.peak.max(counts.stats.active);
        for key in &keys {
            *counts.clients.entry(key.clone()).or_default() += 1;
            if buffered {
                *counts.buffered.entry(key.clone()).or_default() += 1;
            }
        }
        Ok(Permit {
            owner: self.clone(),
            keys,
            buffered,
        })
    }

    pub fn stats(&self) -> Stats {
        crate::admission::recover(&self.0.counts, "connection").stats
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut counts = crate::admission::recover(&self.owner.0.counts, "connection");
        counts.stats.active -= 1;
        for key in &self.keys {
            let count = counts.clients.get_mut(key).expect("permit owns client capacity");
            *count -= 1;
            if *count == 0 {
                counts.clients.remove(key);
            }
            if self.buffered {
                let count = counts.buffered.get_mut(key).expect("permit owns buffer share");
                *count -= 1;
                if *count == 0 {
                    counts.buffered.remove(key);
                }
            }
        }
    }
}

/// Anonymous IPv6 clients share a /64; IPv4 clients use their individual address.
pub fn subnet(addr: IpAddr) -> IpNet {
    let addr = addr.to_canonical();
    IpNet::new(addr, if addr.is_ipv4() { 32 } else { 64 })
        .expect("valid prefix length")
        .trunc()
}

#[cfg(test)]
mod tests {
    #[test]
    fn poisoned_counts_keep_admitting() {
        let connections = super::Connections::new(4, 4, vec![]);
        let held = connections.clone();
        std::thread::spawn(move || {
            let _counts = held.0.counts.lock().unwrap();
            panic!("bug under the lock");
        })
        .join()
        .unwrap_err();
        drop(connections.acquire("192.0.2.1:1".parse().unwrap()).unwrap());
        assert_eq!(connections.stats().active, 0);
        assert!(!connections.0.counts.is_poisoned(), "recovery must be reported once");
    }
}

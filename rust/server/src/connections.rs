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
    clients: HashMap<IpNet, usize>,
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
    key: Option<IpNet>,
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
        let addr = peer.ip().to_canonical();
        // A trusted proxy shares its socket pool across clients. Its connections
        // still consume global capacity; request policy later attributes clients.
        let key =
            (!self.0.trusted.iter().any(|prefix| prefix.contains(&addr))).then(|| subnet(addr));
        let mut counts = self.0.counts.lock().expect("connection counts poisoned");
        if counts.stats.active >= self.0.global_max {
            counts.stats.rejected_global = counts.stats.rejected_global.saturating_add(1);
            return Err(Refusal::GlobalFull);
        }
        if key.is_some_and(|key| {
            counts.clients.get(&key).copied().unwrap_or_default() >= self.0.client_max
        }) {
            counts.stats.rejected_client = counts.stats.rejected_client.saturating_add(1);
            return Err(Refusal::ClientFull);
        }
        counts.stats.active += 1;
        counts.stats.peak = counts.stats.peak.max(counts.stats.active);
        if let Some(key) = key {
            *counts.clients.entry(key).or_default() += 1;
        }
        Ok(Permit {
            owner: self.clone(),
            key,
        })
    }

    pub fn stats(&self) -> Stats {
        self.0
            .counts
            .lock()
            .expect("connection counts poisoned")
            .stats
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut counts = self
            .owner
            .0
            .counts
            .lock()
            .expect("connection counts poisoned");
        counts.stats.active -= 1;
        if let Some(key) = self.key {
            let count = counts
                .clients
                .get_mut(&key)
                .expect("permit owns client capacity");
            *count -= 1;
            if *count == 0 {
                counts.clients.remove(&key);
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

//! The server's buffer budget, drawn on by QUIC and HTTP/2 connection state, endpoint buffers and the download block.
use quinn::SharedBudget;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Debug)]
pub(super) struct MemoryBudget {
    pub(super) limit: usize,
    used: AtomicUsize,
    held_back: AtomicBool,
}

impl MemoryBudget {
    pub(super) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
            held_back: AtomicBool::new(false),
        })
    }

    pub(super) fn lease(self: &Arc<Self>, bytes: usize) -> Option<Lease> {
        self.try_charge(bytes).then(|| Lease {
            budget: self.clone(),
            bytes,
        })
    }

    #[cfg(test)]
    pub(super) fn available(&self) -> usize {
        self.limit - self.used.load(Ordering::Relaxed)
    }

    pub(super) fn under_pressure(&self) -> bool {
        self.used.load(Ordering::Relaxed) >= self.limit / 4
    }

    pub(super) fn has_headroom(&self) -> bool {
        let used = self.used.load(Ordering::Relaxed);
        let headroom = used < self.limit / 4 * 3;
        // Reported recovery waits for five eighths, so usage hovering at the threshold cannot flood the log.
        let held_back = self.held_back.load(Ordering::Relaxed);
        let changed = if held_back {
            used < self.limit / 8 * 5
        } else {
            !headroom
        };
        if changed
            && self
                .held_back
                .compare_exchange(held_back, !held_back, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            crate::log!(
                "[gm:memory] window growth {}: {used} of {} buffer bytes in use",
                if held_back {
                    "resumed"
                } else {
                    "held back by memory pressure"
                },
                self.limit
            );
        }
        headroom
    }
}

impl SharedBudget for MemoryBudget {
    fn try_charge(&self, bytes: usize) -> bool {
        self.used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes).filter(|&used| used <= self.limit)
            })
            .is_ok()
    }

    fn refund(&self, bytes: usize) {
        self.used.fetch_sub(bytes, Ordering::Relaxed);
    }
}

impl h2::SharedBudget for MemoryBudget {
    fn try_charge(&self, bytes: usize) -> bool {
        SharedBudget::try_charge(self, bytes)
    }

    fn refund(&self, bytes: usize) {
        SharedBudget::refund(self, bytes);
    }
}

#[derive(Debug)]
pub(super) struct Lease {
    pub(super) budget: Arc<MemoryBudget>,
    pub(super) bytes: usize,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.budget.refund(self.bytes);
    }
}

/// The receive-window credit each client may hold across its connections. A client's share of the budget is its
/// share of connection capacity, never less than one HTTP/3 window; as in admission, each wider key (an IPv6 /56
/// and /48, or a login's principal) may hold twice the one before it.
#[derive(Debug)]
pub(super) struct ClientCredit {
    share: usize,
    held: Mutex<HashMap<String, usize>>,
}

impl ClientCredit {
    pub(super) fn new(
        limit: usize,
        max_connections: usize,
        max_connections_per_client: usize,
        floor: usize,
    ) -> Arc<Self> {
        let clients = max_connections_per_client.min(max_connections) as u128;
        let share = (limit as u128 * clients / max_connections.max(1) as u128) as usize;
        Arc::new(Self {
            share: share.max(floor),
            held: Mutex::default(),
        })
    }

    /// Charges `bytes` to every key of an admitted client, or to none if any would pass its share.
    pub(super) fn claim(self: &Arc<Self>, keys: &[String], bytes: usize) -> Option<CreditClaim> {
        let mut held = crate::admission::recover(&self.held, "receive credit");
        let fits = keys.iter().enumerate().all(|(index, key)| {
            let share = self.share.saturating_mul(1 << index.min(usize::BITS as usize - 1));
            held.get(key).copied().unwrap_or_default().saturating_add(bytes) <= share
        });
        if !fits {
            return None;
        }
        for key in keys {
            *held.entry(key.clone()).or_default() += bytes;
        }
        Some(CreditClaim {
            credit: self.clone(),
            keys: keys.to_vec(),
            bytes,
        })
    }
}

/// Released once, when the connection that holds it can no longer be sent the credit it covers.
#[derive(Debug)]
pub(super) struct CreditClaim {
    credit: Arc<ClientCredit>,
    keys: Vec<String>,
    bytes: usize,
}

impl Drop for CreditClaim {
    fn drop(&mut self) {
        let mut held = crate::admission::recover(&self.credit.held, "receive credit");
        for key in &self.keys {
            let bytes = held.get_mut(key).expect("claimed keys are held");
            *bytes -= self.bytes;
            if *bytes == 0 {
                held.remove(key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ClientCredit;

    #[test]
    fn a_client_share_follows_connection_capacity_and_doubles_for_wider_keys() {
        // 64 of 4096 connections: 1/64 of the budget, or the floor when that is more.
        assert_eq!(ClientCredit::new(64 << 20, 4096, 64, 1).share, 1 << 20);
        assert_eq!(ClientCredit::new(64 << 20, 4096, 64, 3 << 20).share, 3 << 20);
        assert_eq!(ClientCredit::new(64 << 20, 4, 64, 1).share, 64 << 20);
        let credit = ClientCredit::new(64 << 20, 4096, 64, 1);
        let claim =
            |address: &str| credit.claim(&crate::client_address::client_keys(address.parse().unwrap()), 1 << 20);
        let first = claim("2001:db8:1:1::1").unwrap();
        assert!(claim("2001:db8:1:1::2").is_none(), "its /64 holds the share");
        let mut held = vec![claim("2001:db8:1:2::1").unwrap()];
        assert!(claim("2001:db8:1:3::1").is_none(), "its /56 holds twice the share");
        held.push(claim("2001:db8:1:100::1").unwrap());
        held.push(claim("2001:db8:1:101::1").unwrap());
        assert!(
            claim("2001:db8:1:200::1").is_none(),
            "its /48 holds four times the share"
        );
        assert!(claim("2001:db8:2::1").is_some());
        drop(first);
        held.push(claim("2001:db8:1:1::2").unwrap());
        let logins = [
            ["login:a", "principal:p"],
            ["login:b", "principal:p"],
            ["login:c", "principal:p"],
        ]
        .map(|keys| keys.map(String::from));
        held.push(credit.claim(&logins[0], 1 << 20).unwrap());
        held.push(credit.claim(&logins[1], 1 << 20).unwrap());
        assert!(
            credit.claim(&logins[2], 1 << 20).is_none(),
            "a principal holds twice a login's share"
        );
        drop(held);
        assert!(credit.held.lock().unwrap().is_empty());
    }
}

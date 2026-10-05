//! The buffer budget QUIC and HTTP/2 connection state and the download block draw on (`GM_MAX_BUFFER_BYTES`).

use crate::log::Latch;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// One shared byte limit; clones share it.
#[derive(Debug, Clone)]
pub struct Budget(Arc<Shared>);

#[derive(Debug)]
struct Shared {
    limit: usize,
    used: AtomicUsize,
    /// Endpoint buffers within `used`, which grow with the host's cores rather than its load.
    reserved: AtomicUsize,
    held_back: Latch,
}

/// How full the budget is, past its reservations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    Normal,
    /// A quarter is used: unvalidated QUIC handshakes need Retry.
    Retry,
    /// Three quarters are used: no receive or send window grows.
    HoldBack,
}

/// A snapshot for tests and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub limit: usize,
    pub used: usize,
    pub reserved: usize,
}

impl Budget {
    pub fn new(limit: usize) -> Self {
        Self(Arc::new(Shared {
            limit,
            used: AtomicUsize::new(0),
            reserved: AtomicUsize::new(0),
            held_back: Latch::default(),
        }))
    }

    /// Charges `bytes` until the lease drops, if they fit.
    pub fn lease(&self, bytes: usize) -> Option<Lease> {
        self.0
            .charge(bytes)
            .then(|| Lease { budget: self.clone(), bytes, kind: Kind::Load })
    }

    /// Charges endpoint buffers, which the pressure thresholds and the clients' half leave out.
    pub fn reserve(&self, bytes: usize) -> Option<Lease> {
        let lease = self
            .0
            .charge(bytes)
            .then(|| Lease { budget: self.clone(), bytes, kind: Kind::Reserved })?;
        self.0.reserved.fetch_add(bytes, Ordering::Relaxed);
        Some(lease)
    }

    /// The pressure level; logs when growth is held back at three quarters and when usage falls below five eighths.
    pub fn pressure(&self) -> Pressure {
        let (limit, used) = self.0.unreserved();
        if let Some(held_back) = self.0.held_back.update(used >= limit / 4 * 3, used < limit / 8 * 5) {
            let change = if held_back { "held back by memory pressure" } else { "resumed" };
            let Usage { limit, used, .. } = self.usage();
            crate::log!("[gm:memory] window growth {change}: {used} of {limit} buffer bytes in use");
        }
        match used {
            used if used >= limit / 4 * 3 => Pressure::HoldBack,
            used if used >= limit / 4 => Pressure::Retry,
            _ => Pressure::Normal,
        }
    }

    /// The receive-window credit all clients together may hold: half the budget past its reservations.
    pub fn client_windows(&self) -> usize {
        self.0.unreserved().0 / 2
    }

    pub fn usage(&self) -> Usage {
        Usage {
            limit: self.0.limit,
            used: self.0.used.load(Ordering::Relaxed),
            reserved: self.0.reserved.load(Ordering::Relaxed),
        }
    }

    /// The budget as noq charges it.
    pub fn noq(&self) -> Arc<dyn noq::SharedBudget> {
        self.0.clone()
    }

    /// The budget as the h2 fork charges it.
    pub fn h2(&self) -> Arc<dyn h2::SharedBudget> {
        self.0.clone()
    }
}

impl Shared {
    fn charge(&self, bytes: usize) -> bool {
        let fits = |used: usize| used.checked_add(bytes).filter(|&used| used <= self.limit);
        self.used.try_update(Ordering::Relaxed, Ordering::Relaxed, fits).is_ok()
    }

    fn give_back(&self, bytes: usize) {
        self.used.fetch_sub(bytes, Ordering::Relaxed);
    }

    /// The limit past the reservations and what of it is used.
    fn unreserved(&self) -> (usize, usize) {
        let reserved = self.reserved.load(Ordering::Relaxed);
        let used = self.used.load(Ordering::Relaxed).saturating_sub(reserved);
        (self.limit.saturating_sub(reserved), used)
    }
}

/// The forks' `SharedBudget` traits have one shape; both charge the same budget.
macro_rules! shared_budget {
    ($($fork:ident),+) => {$(
        impl $fork::SharedBudget for Shared {
            fn try_charge(&self, bytes: usize) -> bool {
                self.charge(bytes)
            }

            fn refund(&self, bytes: usize) {
                self.give_back(bytes);
            }
        }
    )+};
}

shared_budget!(noq, h2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Load,
    Reserved,
}

/// Bytes charged until dropped.
#[derive(Debug)]
#[must_use]
pub struct Lease {
    budget: Budget,
    bytes: usize,
    kind: Kind,
}

impl Lease {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.kind == Kind::Reserved {
            self.budget.0.reserved.fetch_sub(self.bytes, Ordering::Relaxed);
        }
        self.budget.0.give_back(self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leases_charge_until_dropped_and_never_past_the_limit() {
        let budget = Budget::new(100);
        let lease = budget.lease(60).unwrap();
        assert!(budget.lease(41).is_none());
        let rest = budget.lease(40).unwrap();
        assert_eq!(budget.usage(), Usage { limit: 100, used: 100, reserved: 0 });
        assert!(budget.lease(1).is_none() && budget.lease(usize::MAX).is_none());
        drop((lease, rest));
        assert_eq!(budget.usage().used, 0);
    }

    #[test]
    fn pressure_counts_only_what_reservations_leave() {
        let budget = Budget::new(1000);
        let reserved = budget.reserve(200).unwrap();
        assert_eq!(budget.pressure(), Pressure::Normal, "an idle server with endpoints is under no pressure");
        assert_eq!(budget.client_windows(), 400);
        let quarter = budget.lease(199).unwrap();
        assert_eq!(budget.pressure(), Pressure::Normal);
        let one = budget.lease(1).unwrap();
        assert_eq!(budget.pressure(), Pressure::Retry);
        let more = budget.lease(400).unwrap();
        assert_eq!(budget.pressure(), Pressure::HoldBack);
        drop((more, one, quarter));
        assert_eq!(budget.pressure(), Pressure::Normal);
        drop(reserved);
        assert_eq!(
            (budget.usage(), budget.client_windows()),
            (Usage { limit: 1000, used: 0, reserved: 0 }, 500)
        );
    }

    #[test]
    fn the_hold_back_report_needs_usage_below_five_eighths_to_end() {
        let budget = Budget::new(800);
        let held = budget.lease(600).unwrap();
        assert_eq!(budget.pressure(), Pressure::HoldBack);
        assert!(budget.0.held_back.on());
        drop(held);
        let hovering = budget.lease(500).unwrap();
        assert_eq!(budget.pressure(), Pressure::Retry);
        assert!(budget.0.held_back.on(), "five eighths still count as held back");
        drop(hovering);
        let _low = budget.lease(499).unwrap();
        budget.pressure();
        assert!(!budget.0.held_back.on());
    }

    #[test]
    fn both_forks_charge_and_refund_the_same_budget() {
        let budget = Budget::new(100);
        let (noq, h2) = (budget.noq(), budget.h2());
        assert!(noq.try_charge(70) && !h2.try_charge(31) && h2.try_charge(30));
        assert_eq!(budget.usage().used, 100);
        assert!(budget.lease(1).is_none(), "fork charges count against leases");
        noq.refund(30);
        h2.refund(70);
        assert_eq!(budget.usage().used, 0);
    }
}

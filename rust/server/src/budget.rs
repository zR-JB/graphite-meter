//! The server's buffer budget, drawn on by QUIC and HTTP/2 connection state, endpoint buffers and the download block.
use quinn::SharedBudget;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
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

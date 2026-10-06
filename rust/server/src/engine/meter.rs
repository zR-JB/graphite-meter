//! Throughput for the verbose logs: the bytes and running transfers of one direction.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

/// One direction's counts while verbose logs are on; clones share them.
#[derive(Debug, Clone, Default)]
pub struct Meter(Option<Arc<Counts>>);

#[derive(Debug, Default)]
struct Counts {
    bytes: AtomicU64,
    transfers: AtomicUsize,
}

/// A running transfer, counted until dropped.
#[derive(Debug)]
pub struct Transfer(Arc<Counts>);

impl Meter {
    /// A meter that counts only when `enabled`.
    pub fn new(enabled: bool) -> Self {
        Self(enabled.then(Arc::default))
    }

    /// A running transfer, one per lane, which the lane's streams share.
    pub fn open(&self) -> Option<Arc<Transfer>> {
        let counts = self.0.as_ref()?;
        counts.transfers.fetch_add(1, Ordering::Relaxed);
        Some(Arc::new(Transfer(counts.clone())))
    }

    /// The line for the bytes counted since the last over `window`, such as `[gm:server:download] 1.20 Gbit/s ·
    /// 2 conns · 150.00 MB this window`, counting a conn per running lane; none without bytes or transfers.
    pub fn line(&self, direction: &str, window: Duration) -> Option<String> {
        let counts = self.0.as_ref()?;
        let bytes = counts.bytes.swap(0, Ordering::Relaxed);
        let transfers = counts.transfers.load(Ordering::Relaxed);
        if bytes == 0 && transfers == 0 {
            return None;
        }
        let gbits = bytes as f64 * 8.0 / window.as_secs_f64() / 1e9;
        let megabytes = bytes as f64 / 1e6;
        Some(format!(
            "[gm:server:{direction}] {gbits:.2} Gbit/s · {transfers} conns · {megabytes:.2} MB this window"
        ))
    }
}

impl Transfer {
    pub fn record(&self, bytes: usize) {
        self.0.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        self.0.transfers.fetch_sub(1, Ordering::Relaxed);
    }
}

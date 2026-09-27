use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::Duration;

#[derive(Default)]
struct Counts {
    bytes: AtomicU64,
    active: AtomicUsize,
}

#[derive(Clone, Default)]
pub(crate) struct Meter(Option<Arc<Counts>>);

pub(crate) struct Transfer(Arc<Counts>);

impl Meter {
    pub(crate) fn new(enabled: bool) -> Self {
        Self(enabled.then(|| Arc::new(Counts::default())))
    }

    pub(crate) fn open(&self) -> Option<Transfer> {
        self.0.as_ref().map(|counts| {
            counts.active.fetch_add(1, Ordering::Relaxed);
            Transfer(counts.clone())
        })
    }

    pub(crate) fn log(&self, direction: &str, window: Duration) {
        let Some(counts) = &self.0 else { return };
        let bytes = counts.bytes.swap(0, Ordering::Relaxed);
        let active = counts.active.load(Ordering::Relaxed);
        if bytes != 0 || active != 0 {
            eprintln!(
                "[gm:server:{direction}] {:.2} Gbit/s · {active} conns · {:.2} MB this window",
                bytes as f64 * 8.0 / window.as_secs_f64() / 1e9,
                bytes as f64 / 1e6,
            );
        }
    }
}

impl Transfer {
    pub(crate) fn record(&self, bytes: usize) {
        self.0.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

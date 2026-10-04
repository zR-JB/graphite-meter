use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{runtime::Handle, sync::watch};

/// Current-thread runtimes on threads of their own, one for each worker of the multi-thread runtime that makes them,
/// so that each connection's tasks stay on one thread: its buffers are allocated and freed there, and none of its
/// work waits on a handoff between threads. Their threads end with the pool; a current-thread runtime makes none.
pub struct Pool {
    pub runtimes: Vec<Handle>,
    turn: AtomicUsize,
    _running: watch::Sender<()>,
}

impl Pool {
    pub fn new() -> std::io::Result<Self> {
        let workers = Handle::try_current().map_or(1, |runtime| runtime.metrics().num_workers());
        let (running, ended) = watch::channel(());
        let runtimes = (0..if workers > 1 { workers } else { 0 })
            .map(|index| {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                let (handle, mut ended) = (runtime.handle().clone(), ended.clone());
                std::thread::Builder::new()
                    .name(format!("gm-worker-{index}"))
                    .spawn(move || runtime.block_on(ended.changed()))?;
                Ok(handle)
            })
            .collect::<std::io::Result<_>>()?;
        Ok(Self {
            runtimes,
            turn: AtomicUsize::new(0),
            _running: running,
        })
    }

    /// The runtimes in turn, or the caller's when the pool has none.
    pub fn next(&self) -> Handle {
        let turn = self.turn.fetch_add(1, Ordering::Relaxed);
        match self.runtimes.get(turn % self.runtimes.len().max(1)) {
            Some(runtime) => runtime.clone(),
            None => Handle::current(),
        }
    }
}

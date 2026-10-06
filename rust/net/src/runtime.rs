//! Current-thread runtimes that keep each connection's tasks, and its buffers, on one thread.
use std::{
    io,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{
    runtime::{Handle, RuntimeFlavor},
    sync::watch,
};

/// Pinned current-thread runtimes, each on a thread of its own, or none; their threads end with the pool.
pub struct Pool {
    runtimes: Vec<Handle>,
    turn: AtomicUsize,
    _running: watch::Sender<()>,
}

impl Pool {
    /// One pinned runtime per worker of `runtime`, none for one worker; a current-thread runtime is refused.
    pub fn beside(runtime: &Handle) -> io::Result<Self> {
        if runtime.runtime_flavor() == RuntimeFlavor::CurrentThread {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "pinned runtimes need a multi-thread runtime"));
        }
        let workers = runtime.metrics().num_workers();
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
            .collect::<io::Result<_>>()?;
        Ok(Self { runtimes, turn: AtomicUsize::new(0), _running: running })
    }

    /// No pinned runtimes: work runs on the caller's runtime.
    pub fn inline() -> Self {
        Self {
            runtimes: Vec::new(),
            turn: AtomicUsize::new(0),
            _running: watch::channel(()).0,
        }
    }

    /// The runtimes in turn, or the caller's when the pool has none.
    pub fn next(&self) -> Handle {
        let turn = self.turn.fetch_add(1, Ordering::Relaxed);
        match self.runtimes.get(turn % self.runtimes.len().max(1)) {
            Some(runtime) => runtime.clone(),
            None => Handle::current(),
        }
    }

    pub fn runtimes(&self) -> &[Handle] {
        &self.runtimes
    }
}

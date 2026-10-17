//! Runs current-thread runtimes that keep each connection's tasks, and its buffers, on one thread.
use std::{
    io,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{
    runtime::{Handle, RuntimeFlavor},
    sync::watch,
};

/// A pool runs its pinned threads this many nice steps below the caller's threads on Linux.
const PINNED_NICE: i32 = 5;

/// Holds pinned current-thread runtimes for bulk transfers, niced on Linux so the caller's threads and a probing
/// pool's latency runtime preempt them. Their threads end with the pool.
#[derive(Default)]
pub struct Pool {
    runtimes: Vec<Handle>,
    latency: Option<Handle>,
    turn: AtomicUsize,
    _running: watch::Sender<()>,
}

impl Pool {
    /// Spawns one pinned runtime per worker of `runtime`, none for one worker, and refuses a current-thread runtime.
    pub fn beside(runtime: &Handle) -> io::Result<Self> {
        Self::spawn(runtime, false)
    }

    /// Spawns what `beside` does, plus a runtime of its own for latency probes and their channels.
    pub fn probing(runtime: &Handle) -> io::Result<Self> {
        Self::spawn(runtime, true)
    }

    fn spawn(runtime: &Handle, probing: bool) -> io::Result<Self> {
        if runtime.runtime_flavor() == RuntimeFlavor::CurrentThread {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "pinned runtimes need a multi-thread runtime"));
        }
        let workers = runtime.metrics().num_workers();
        let (running, ended) = watch::channel(());
        let thread = |name: String, niced: bool| -> io::Result<Handle> {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            let (handle, mut ended) = (runtime.handle().clone(), ended.clone());
            std::thread::Builder::new().name(name).spawn(move || {
                if niced {
                    lower_priority();
                }
                runtime.block_on(ended.changed())
            })?;
            Ok(handle)
        };
        let runtimes = (0..if workers > 1 { workers } else { 0 })
            .map(|index| thread(format!("gm-worker-{index}"), true))
            .collect::<io::Result<_>>()?;
        let latency = probing.then(|| thread("gm-latency".into(), false)).transpose()?;
        Ok(Self {
            runtimes,
            latency,
            turn: AtomicUsize::new(0),
            _running: running,
        })
    }

    pub fn inline() -> Self {
        Self::default()
    }

    /// Hands out the runtimes in turn, or the caller's when the pool has none.
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

    /// Hands out the latency runtime, or the caller's in a pool without one.
    pub fn latency(&self) -> Handle {
        self.latency.clone().unwrap_or_else(Handle::current)
    }
}

/// Lowers the calling thread's priority by `PINNED_NICE`. Raising nice needs no privilege, and failing leaves it.
fn lower_priority() {
    cfg_select! {
        target_os = "linux" => {
            let thread = Some(rustix::thread::gettid());
            let nice = rustix::process::getpriority_process(thread).unwrap_or(0);
            let _ = rustix::process::setpriority_process(thread, nice.saturating_add(PINNED_NICE));
        }
        _ => {}
    }
}

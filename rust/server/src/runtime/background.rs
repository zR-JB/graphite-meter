//! Work beside the listeners: returning freed memory once the server is idle, and the verbose logs.

use crate::{app::App, log};
use graphite_meter_net::Pool;
use std::time::Duration;
use tokio::time::{MissedTickBehavior, interval, timeout};

/// The server returns freed memory once it has had no connection for this long.
const IDLE_RELEASE: Duration = Duration::from_secs(2);
/// Verbose logs report transfer rates every second and the admission counters every thirty.
const TRANSFER_LOG: Duration = Duration::from_secs(1);
const ADMISSION_LOG: Duration = Duration::from_secs(30);

/// Calls `release` once per idle period, `IDLE_RELEASE` after the last connection closed.
pub(super) async fn release_when_idle(app: &App, release: impl Fn()) {
    loop {
        app.quotas().idle().await;
        // Each close that empties the server again restarts the wait.
        while timeout(IDLE_RELEASE, app.quotas().idle()).await.is_ok() {}
        if app.quotas().connections() == 0 {
            release();
        }
    }
}

/// Returns freed allocator pages to the OS from this thread's heap and every pool thread's: mimalloc returns them
/// only while the freeing thread allocates, which an idle server's threads do not.
pub(super) fn release_memory(pool: &Pool) {
    collect();
    for runtime in pool.runtimes() {
        runtime.spawn(async { collect() });
    }
}

/// Collects the calling thread's heap and purges every arena.
fn collect() {
    #[cfg(target_env = "musl")]
    rustfs_mimalloc::heap::Heap::main().collect(true);
}

/// Logs transfer rates every second and the admission counters every thirty seconds.
pub(super) async fn log_verbose(app: &App) {
    let (mut transfers, mut admission) = (interval(TRANSFER_LOG), interval(ADMISSION_LOG));
    transfers.set_missed_tick_behavior(MissedTickBehavior::Skip);
    admission.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last = transfers.tick().await;
    admission.tick().await;
    loop {
        tokio::select! {
            now = transfers.tick() => {
                for line in app.transfer_lines(now - last) {
                    log!("{line}");
                }
                last = now;
            }
            _ = admission.tick() => log!("{}", app.quotas().admission()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{self, Loaded},
        limits::Transport,
    };
    use std::{
        ffi::OsString,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use tokio::time::advance;
    use tokio_util::sync::CancellationToken;

    fn app() -> App {
        let none = |_: &str| None::<OsString>;
        let Ok(Loaded::Config(config)) = config::load(none, Vec::<OsString>::new(), &mut Vec::new()) else {
            panic!("the default configuration loads");
        };
        App::new(*config, CancellationToken::new()).unwrap()
    }

    /// Lets the release loop run up to its next wait.
    async fn settle() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn freed_memory_is_released_two_seconds_after_the_last_connection_closes() {
        let app = app();
        let releases = AtomicUsize::new(0);
        let peer = "192.0.2.1".parse().unwrap();
        let first = app.connection(peer, Transport::Tcp).unwrap();
        let second = app.connection(peer, Transport::Quic).unwrap();
        let released = || releases.load(Ordering::Relaxed);
        let releasing = release_when_idle(&app, || {
            releases.fetch_add(1, Ordering::Relaxed);
        });
        let test = async {
            drop(first);
            settle().await;
            advance(IDLE_RELEASE * 2).await;
            settle().await;
            assert_eq!(released(), 0, "a connection remains");
            drop(second);
            settle().await;
            advance(IDLE_RELEASE - Duration::from_millis(1)).await;
            settle().await;
            assert_eq!(released(), 0);
            advance(Duration::from_millis(1)).await;
            settle().await;
            assert_eq!(released(), 1, "two seconds after the last connection closed");

            let again = app.connection(peer, Transport::Tcp).unwrap();
            advance(IDLE_RELEASE * 3).await;
            settle().await;
            assert_eq!(released(), 1, "nothing is released while a connection is open");
            drop(again);
            settle().await;
            advance(IDLE_RELEASE).await;
            settle().await;
            assert_eq!(released(), 2);
            advance(IDLE_RELEASE * 3).await;
            settle().await;
            assert_eq!(released(), 2, "once per idle period");

            for _ in 0..4 {
                drop(app.connection(peer, Transport::Tcp).unwrap());
                settle().await;
                advance(IDLE_RELEASE - Duration::from_millis(500)).await;
                settle().await;
            }
            assert_eq!(released(), 2, "short connections every 1.5 s release nothing");
            advance(Duration::from_millis(500)).await;
            settle().await;
            assert_eq!(released(), 3, "two seconds after the last of them");
        };
        tokio::select! {
            () = releasing => unreachable!("the release loop runs until the server stops"),
            () = test => {}
        }
    }
}

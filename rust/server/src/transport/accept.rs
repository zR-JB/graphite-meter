//! The one accept-and-drain loop: connection holds, retries after failed accepts, connections on pinned runtimes, and
//! the drain at shutdown.

use crate::{limits::Hold, log};
use futures_util::FutureExt;
use graphite_meter_proto::duration;
use std::{any::Any, future::Future, io, net::SocketAddr, panic::AssertUnwindSafe, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    runtime::Handle,
    task::JoinSet,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

/// Running connections finish within this once shutdown begins.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// A failed accept is retried after a delay doubling from the first to the last.
const RETRY_FIRST: Duration = Duration::from_millis(5);
const RETRY_LAST: Duration = Duration::from_secs(1);

/// A socket connections are accepted from.
pub trait Listen {
    type Connection: Send + 'static;

    /// The next connection, or `None` for an attempt the listener answered itself, such as with a Retry.
    fn accept(&mut self) -> impl Future<Output = io::Result<Option<(Self::Connection, SocketAddr)>>> + Send;

    /// The socket in accept failure lines, such as `tcp [::]:7246`.
    fn name(&self) -> String;

    /// What closes the connections left once the drain ended; the listener stopped before.
    fn closer(&self) -> impl Future<Output = ()> + Send + 'static {
        async {}
    }
}

impl Listen for TcpListener {
    type Connection = TcpStream;

    async fn accept(&mut self) -> io::Result<Option<(TcpStream, SocketAddr)>> {
        TcpListener::accept(self).await.map(Some)
    }

    fn name(&self) -> String {
        self.local_addr()
            .map_or_else(|_| "tcp".into(), |address| format!("tcp {address}"))
    }
}

/// Accepts until `shutdown`, then closes the listener at once and drains; ends early if the socket stops listening.
pub async fn serve<L, F>(
    mut listener: L,
    runtime: impl Fn() -> Handle,
    shutdown: &CancellationToken,
    hold: impl Fn(SocketAddr) -> Option<Hold>,
    serve: impl Fn(L::Connection, SocketAddr) -> F,
) -> io::Result<()>
where
    L: Listen,
    F: Future<Output = ()> + Send + 'static,
{
    let mut connections = JoinSet::new();
    let mut delay = Duration::ZERO;
    let result = loop {
        let accepted = tokio::select! {
            biased;
            () = shutdown.cancelled() => break Ok(()),
            Some(_) = connections.join_next() => continue,
            accepted = listener.accept() => accepted,
        };
        // An accept that is always ready, as noq's under a flood, still leaves the runtime to its other tasks.
        tokio::task::consume_budget().await;
        let (connection, peer) = match accepted {
            Ok(Some(accepted)) => accepted,
            Ok(None) => continue,
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => break Err(error),
            Err(error) => {
                delay = (delay * 2).clamp(RETRY_FIRST, RETRY_LAST);
                let (name, retry) = (listener.name(), duration::format(delay));
                log!("http: Accept error: accept {name}: {error}; retrying in {retry}");
                tokio::select! {
                    () = shutdown.cancelled() => break Ok(()),
                    () = sleep(delay) => continue,
                }
            }
        };
        delay = Duration::ZERO;
        let peer = SocketAddr::new(peer.ip().to_canonical(), peer.port());
        let Some(held) = hold(peer) else {
            continue;
        };
        let serving = AssertUnwindSafe(serve(connection, peer)).catch_unwind();
        let task = async move {
            let _held = held;
            if let Err(panic) = serving.await {
                log!("http: panic serving {peer}: {}", message(&*panic));
            }
        };
        connections.spawn_on(task, &runtime());
    };
    let close = listener.closer();
    drop(listener);
    let _ = timeout(SHUTDOWN_GRACE, async { while connections.join_next().await.is_some() {} }).await;
    connections.shutdown().await;
    close.await;
    result
}

fn message(panic: &(dyn Any + Send)) -> &str {
    match (panic.downcast_ref::<&str>(), panic.downcast_ref::<String>()) {
        (Some(text), _) => text,
        (_, Some(text)) => text,
        _ => "panic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Quota;
    use crate::peer::ClientKeys;
    use graphite_meter_net::Pool;
    use std::sync::Arc;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::Notify,
        task::JoinHandle,
    };

    /// A loop on a fresh local listener whose connections run `serve`, holding shares of `quota`.
    async fn start<F>(
        quota: Quota,
        serve: impl Fn(TcpStream) -> F + Send + Sync + 'static,
    ) -> (SocketAddr, CancellationToken, JoinHandle<io::Result<()>>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let task = tokio::spawn(async move {
            let pool = Pool::new().unwrap();
            let hold = |peer: SocketAddr| quota.acquire(&ClientKeys::address(peer.ip()), 1).ok();
            super::serve(listener, || pool.next(), &stop, hold, |socket, _| serve(socket)).await
        });
        (address, shutdown, task)
    }

    async fn greeting(address: SocketAddr) -> io::Result<String> {
        let mut socket = TcpStream::connect(address).await?;
        let mut text = String::new();
        socket.read_to_string(&mut text).await?;
        Ok(text)
    }

    async fn greet(mut socket: TcpStream) {
        let _ = socket.write_all(b"hello").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panic_in_one_connection_leaves_the_listener_serving() {
        let quota = Quota::new(10, 10);
        let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let serve = move |socket: TcpStream| {
            let panics = first.swap(false, std::sync::atomic::Ordering::Relaxed);
            async move {
                assert!(!panics, "connection bug");
                greet(socket).await;
            }
        };
        let (address, shutdown, task) = start(quota.clone(), serve).await;
        assert_eq!(greeting(address).await.unwrap(), "", "the panicking connection closes");
        assert_eq!(greeting(address).await.unwrap(), "hello");
        let released = async {
            while quota.usage().active > 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        };
        timeout(Duration::from_secs(1), released)
            .await
            .expect("every connection released its hold");
        shutdown.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stopping_listener_refuses_connections_while_it_drains() {
        let release = Arc::new(Notify::new());
        let (started, started_rx) = tokio::sync::mpsc::unbounded_channel();
        let serve = {
            let release = release.clone();
            move |socket: TcpStream| {
                let (release, started) = (release.clone(), started.clone());
                async move {
                    let _ = started.send(());
                    release.notified().await;
                    greet(socket).await;
                }
            }
        };
        let (address, shutdown, task) = start(Quota::new(10, 10), serve).await;
        let mut draining = TcpStream::connect(address).await.unwrap();
        let mut started_rx = started_rx;
        started_rx.recv().await.unwrap();
        shutdown.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!task.is_finished(), "the running connection drains");
        let refused = TcpStream::connect(address).await.unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);
        drop(TcpListener::bind(address).await.expect("the address is free again"));
        release.notify_one();
        let mut text = String::new();
        draining.read_to_string(&mut text).await.unwrap();
        assert_eq!(text, "hello", "the draining connection finished its work");
        task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn connections_left_after_the_grace_are_cut() {
        let quota = Quota::new(10, 10);
        let (address, shutdown, task) = start(quota.clone(), |socket: TcpStream| async move {
            std::future::pending::<()>().await;
            drop(socket);
        })
        .await;
        let mut socket = TcpStream::connect(address).await.unwrap();
        while quota.usage().active == 0 {
            tokio::task::yield_now().await;
        }
        let stopped = tokio::time::Instant::now();
        shutdown.cancel();
        task.await.unwrap().unwrap();
        assert_eq!(stopped.elapsed(), SHUTDOWN_GRACE);
        assert_eq!(socket.read(&mut [0; 1]).await.unwrap(), 0);
    }

    /// A listener whose accepts fail `failures` times, recording when each was tried.
    struct Failing {
        failures: usize,
        tried: Arc<std::sync::Mutex<Vec<tokio::time::Instant>>>,
    }

    impl Listen for Failing {
        type Connection = TcpStream;

        async fn accept(&mut self) -> io::Result<Option<(TcpStream, SocketAddr)>> {
            crate::lock(&self.tried).push(tokio::time::Instant::now());
            if self.failures == 0 {
                return std::future::pending().await;
            }
            self.failures -= 1;
            Err(io::Error::other("too many open files"))
        }

        fn name(&self) -> String {
            "tcp test".into()
        }
    }

    /// A listener flooded with attempts it answers itself, each ready at once.
    struct Flooded;

    impl Listen for Flooded {
        type Connection = ();

        async fn accept(&mut self) -> io::Result<Option<((), SocketAddr)>> {
            Ok(None)
        }

        fn name(&self) -> String {
            "flooded".into()
        }
    }

    #[test]
    fn a_flood_of_attempts_answered_at_once_leaves_the_runtime_to_its_other_tasks() {
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let shutdown = CancellationToken::new();
                let stop = shutdown.clone();
                let serving = tokio::spawn(async move {
                    super::serve(Flooded, Handle::current, &stop, |_| None, |(), _| async {}).await
                });
                tokio::task::yield_now().await;
                shutdown.cancel();
                serving.await.unwrap().unwrap();
            });
            let _ = done.send(());
        });
        let stopped = finished.recv_timeout(Duration::from_secs(5));
        assert!(stopped.is_ok(), "the stop ran beside the flood and ended the loop");
    }

    /// A listener with one connection whose closer reports when it ran and whether that connection was gone.
    struct Closing {
        accepted: bool,
        cut: Arc<std::sync::atomic::AtomicBool>,
        closed: Arc<std::sync::Mutex<Option<(tokio::time::Instant, bool)>>>,
    }

    impl Listen for Closing {
        type Connection = ();

        async fn accept(&mut self) -> io::Result<Option<((), SocketAddr)>> {
            if std::mem::replace(&mut self.accepted, true) {
                return std::future::pending().await;
            }
            Ok(Some(((), "192.0.2.1:1".parse().unwrap())))
        }

        fn name(&self) -> String {
            "test".into()
        }

        fn closer(&self) -> impl Future<Output = ()> + Send + 'static {
            let (cut, closed) = (self.cut.clone(), self.closed.clone());
            async move {
                let gone = cut.load(std::sync::atomic::Ordering::Relaxed);
                *crate::lock(&closed) = Some((tokio::time::Instant::now(), gone));
            }
        }
    }

    /// Records when dropped.
    struct Cut(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for Cut {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_listener_s_closer_runs_once_the_drain_cut_what_was_left() {
        let (cut, closed) = (Arc::default(), Arc::default());
        let listener = Closing {
            accepted: false,
            cut: Arc::clone(&cut),
            closed: Arc::clone(&closed),
        };
        let (shutdown, quota) = (CancellationToken::new(), Quota::new(10, 10));
        let hold = |peer: SocketAddr| quota.acquire(&ClientKeys::address(peer.ip()), 1).ok();
        let serve = |(), _| {
            let cut = Cut(cut.clone());
            async move {
                std::future::pending::<()>().await;
                drop(cut);
            }
        };
        let serving = super::serve(listener, Handle::current, &shutdown, hold, serve);
        let stop = async {
            while quota.usage().active == 0 {
                tokio::task::yield_now().await;
            }
            shutdown.cancel();
        };
        let stopped = tokio::time::Instant::now();
        let (served, ()) = tokio::join!(serving, stop);
        served.unwrap();
        let (at, gone) = crate::lock(&closed).expect("the closer ran");
        assert_eq!(at - stopped, SHUTDOWN_GRACE);
        assert!(gone, "the connection was cut before");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_accept_is_retried_after_a_delay_doubling_to_a_second() {
        let tried = Arc::default();
        let listener = Failing { failures: 10, tried: Arc::clone(&tried) };
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let task = tokio::spawn(async move {
            let pool = Pool::new().unwrap();
            super::serve(listener, || pool.next(), &stop, |_| None, |_, _| async {}).await
        });
        tokio::time::sleep(Duration::from_secs(5)).await;
        shutdown.cancel();
        task.await.unwrap().unwrap();
        let tried = crate::lock(&tried);
        let delays: Vec<_> = tried.windows(2).map(|pair| (pair[1] - pair[0]).as_millis()).collect();
        assert_eq!(delays, [5, 10, 20, 40, 80, 160, 320, 640, 1000, 1000]);
    }

    #[tokio::test]
    async fn a_connection_beyond_its_share_is_closed_unserved() {
        let quota = Quota::new(10, 1);
        let release = Arc::new(Notify::new());
        let serve = {
            let release = release.clone();
            move |socket: TcpStream| {
                let release = release.clone();
                async move {
                    release.notified().await;
                    greet(socket).await;
                }
            }
        };
        let (address, shutdown, task) = start(quota.clone(), serve).await;
        let first = tokio::spawn(greeting(address));
        while quota.usage().active == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(greeting(address).await.unwrap(), "", "the second connection of a client is closed");
        release.notify_one();
        assert_eq!(first.await.unwrap().unwrap(), "hello");
        shutdown.cancel();
        task.await.unwrap().unwrap();
    }
}

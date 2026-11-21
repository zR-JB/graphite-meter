pub mod accept;
pub mod body;
pub mod http1;
pub mod http2;
pub mod lifecycle;
pub mod quic;
pub mod tls;
pub mod websocket;
pub mod webtransport;
mod window;

use crate::log::{Level, RateLimited};
use std::{
    io::{self, IoSlice},
    pin::{Pin, pin},
    task::{Context, Poll, ready},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader, Interest, ReadBuf},
    net::TcpStream,
};
use tokio_rustls::server::TlsStream;

/// Connection failures any peer can cause, such as a refused TLS handshake.
pub(crate) static PEER_FAILURES: RateLimited = RateLimited::new(
    Level::Info,
    "peer",
    "peer connection failures",
    module_path!(),
);

/// Bytes a connection's socket moves before its task lets the other tasks on its pinned runtime run.
const BURST_BYTES: usize = 256 << 10;

/// A connection's socket bytes since its task last waited, which bound how long a bulk transfer holds its thread.
#[derive(Default)]
pub(crate) struct Burst(usize);

impl Burst {
    /// Past `BURST_BYTES`, pending once, woken after every other ready task and the I/O driver ran.
    pub fn poll_turn(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 < BURST_BYTES {
            return Poll::Ready(());
        }
        self.0 = 0;
        // A fresh `yield_now` is pending on its first poll, with the wake deferred past the driver.
        let _ = pin!(tokio::task::yield_now()).poll(cx);
        Poll::Pending
    }

    pub fn moved(&mut self, bytes: usize) {
        self.0 = self.0.saturating_add(bytes);
    }

    /// The socket would block: the task waits anyway, so its next poll starts a new burst.
    pub fn waited(&mut self) {
        self.0 = 0;
    }
}

const FEED_NOTSENT_LOWAT: u32 = 1 << 10;
/// Unsent bytes past this sit in kernel memory without making an HTTP/1.1 transfer faster.
const NOTSENT_LOWAT: u32 = 1 << 20;

/// Caps the bytes a socket keeps unsent, which pile up only once its window is full.
pub(crate) fn bound_unsent(socket: &TcpStream, bytes: u32) {
    #[cfg(target_os = "linux")]
    let _ = socket2::SockRef::from(socket).set_tcp_notsent_lowat(bytes);
}

#[cfg(target_os = "linux")]
pub(crate) mod lowat {
    use nix::{libc, setsockopt_impl, sockopt_impl, sys::socket::setsockopt};
    use std::{future::Future, io, pin::Pin, task::Context, time::Duration};
    use tokio::{
        net::TcpStream,
        time::{Instant, Sleep},
    };

    sockopt_impl!(
        ReadLowWater,
        SetOnly,
        libc::SOL_SOCKET,
        libc::SO_RCVLOWAT,
        libc::c_int
    );

    pub(crate) const UPLOAD_READ_MIN: u64 = 4 << 10;
    pub(crate) const UPLOAD_READ_MAX: u64 = 16 << 10;

    #[derive(Default)]
    pub(crate) enum BulkRead {
        Sampling {
            started: Instant,
            bytes: u64,
            burst: u64,
            short_waits: u8,
            total_waits: u32,
        },
        Batch {
            last: Instant,
            lowat: u64,
        },
        #[default]
        Ordinary,
    }

    impl BulkRead {
        const SMALL_BURST_BYTES: u64 = 4 << 10;
        const SAMPLE_BYTES: u64 = 32 << 10;
        const SAMPLE_WAITS: u8 = 12;
        const SAMPLE_TIME: Duration = Duration::from_millis(25);
        /// Target kernel batching interval: well below `WAIT` so steady transfers stay batched without adding delay.
        const TARGET_WAIT_US: u128 = 2_000;
        /// A short burst of data must not stall the sender.
        pub(crate) const WAIT: Duration = Duration::from_millis(5);

        pub(crate) fn sample() -> Self {
            Self::Sampling {
                started: Instant::now(),
                bytes: 0,
                burst: 0,
                short_waits: 0,
                total_waits: 0,
            }
        }

        pub(crate) fn is_batched(&self) -> bool {
            matches!(self, Self::Batch { .. })
        }

        pub(crate) fn lowat(&self) -> u64 {
            match *self {
                Self::Batch { lowat, .. } => lowat,
                _ => UPLOAD_READ_MIN,
            }
        }

        pub(crate) fn finish(&mut self, socket: &TcpStream) -> io::Result<()> {
            if matches!(self, Self::Batch { .. }) {
                setsockopt(socket, ReadLowWater, &1)?;
            }
            *self = Self::Ordinary;
            Ok(())
        }

        /// Direct async-loop helper: updates sampling state on a read or WouldBlock; returns true if newly batched.
        pub(crate) fn on_read(&mut self, moved: usize) {
            match self {
                Self::Sampling { bytes, burst, .. } => {
                    *bytes += moved as u64;
                    *burst += moved as u64;
                }
                Self::Batch { last, .. } => *last = Instant::now(),
                Self::Ordinary => {}
            }
        }

        pub(crate) fn on_would_block(&mut self, socket: &TcpStream, remaining: u64) {
            if let Self::Sampling {
                started,
                bytes,
                burst,
                short_waits,
                total_waits,
            } = self
            {
                *total_waits = total_waits.saturating_add(1);
                if (1..=Self::SMALL_BURST_BYTES).contains(&std::mem::take(burst)) {
                    *short_waits = short_waits.saturating_add(1);
                }
                if *bytes >= Self::SAMPLE_BYTES {
                    let now = Instant::now();
                    let elapsed = now.saturating_duration_since(*started);
                    let avg_small = *bytes <= u64::from(*total_waits) * Self::SMALL_BURST_BYTES;
                    if *short_waits >= Self::SAMPLE_WAITS
                        && avg_small
                        && elapsed <= Self::SAMPLE_TIME
                    {
                        let elapsed_us = elapsed.as_micros().max(1);
                        let target = ((*bytes as u128 * Self::TARGET_WAIT_US) / elapsed_us)
                            .min(UPLOAD_READ_MAX as u128)
                            as u64;
                        let cap = remaining.saturating_sub(1).min(UPLOAD_READ_MAX);
                        let lowat = target.min(cap);
                        if lowat >= UPLOAD_READ_MIN
                            && remaining > lowat
                            && setsockopt(socket, ReadLowWater, &(lowat as libc::c_int)).is_ok()
                        {
                            *self = Self::Batch { last: now, lowat };
                            return;
                        }
                    }
                    *started = now;
                    *bytes = 0;
                    *short_waits = 0;
                    *total_waits = 0;
                }
            }
        }

        pub(crate) fn reset_after_timeout(&mut self, socket: &TcpStream) {
            let _ = setsockopt(socket, ReadLowWater, &1);
            *self = Self::sample();
        }

        pub(crate) fn observe(
            &mut self,
            socket: &TcpStream,
            cx: &mut Context<'_>,
            mut deadline: Pin<&mut Sleep>,
            moved: usize,
            pending: bool,
            remaining: u64,
        ) -> io::Result<()> {
            match self {
                Self::Sampling { .. } => {
                    if moved != 0 {
                        self.on_read(moved);
                        return Ok(());
                    }
                    if !pending {
                        return Ok(());
                    }
                    self.on_would_block(socket, remaining);
                    if let Self::Batch { last, .. } = *self {
                        deadline.as_mut().reset(last + Self::WAIT);
                        if deadline.as_mut().poll(cx).is_ready() {
                            cx.waker().wake_by_ref();
                        }
                    }
                }
                Self::Batch { last, .. } if moved != 0 => *last = Instant::now(),
                Self::Batch { last, .. } if pending => {
                    if deadline.as_mut().poll(cx).is_pending() {
                        return Ok(());
                    }
                    deadline.as_mut().reset(*last + Self::WAIT);
                    if deadline.as_mut().poll(cx).is_ready() {
                        setsockopt(socket, ReadLowWater, &1)?;
                        *self = Self::sample();
                        cx.waker().wake_by_ref();
                    }
                }
                _ => {}
            }
            Ok(())
        }
    }
}

/// A feed's stream: on a plain socket each write is a record the kernel never appends to, so the unsent bound holds.
pub(crate) struct Feed<S>(pub S);

impl<S: Rehome> Feed<S> {
    pub const RECORDS: bool = S::PLAIN && cfg!(target_os = "linux");

    pub fn new(mut stream: S) -> Self {
        Self::bound(&mut stream, true);
        Self(stream)
    }

    pub fn bound(stream: &mut S, feed: bool) {
        let lowat = if feed {
            FEED_NOTSENT_LOWAT
        } else {
            NOTSENT_LOWAT
        };
        bound_unsent(stream.socket(), lowat);
    }

    pub fn poll_send(
        stream: &mut S,
        cx: &mut Context<'_>,
        bytes: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let socket = stream.socket();
        loop {
            ready!(socket.poll_write_ready(cx))?;
            let sent = socket.try_io(Interest::WRITABLE, || {
                cfg_select! {
                    target_os = "linux" => {
                        use rustix::net::{SendAncillaryBuffer, SendFlags, sendmsg};
                        let flags = SendFlags::EOR | SendFlags::NOSIGNAL;
                        Ok(sendmsg(&*socket, bytes, &mut SendAncillaryBuffer::default(), flags)?)
                    }
                    _ => Err(io::ErrorKind::Unsupported.into()),
                }
            });
            match sent {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                sent => return Poll::Ready(sent),
            }
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Feed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buffer)
    }
}

impl<S: AsyncWrite + Rehome + Unpin> AsyncWrite for Feed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Self::RECORDS {
            true => Self::poll_send(&mut self.0, cx, &[IoSlice::new(bytes)]),
            false => Pin::new(&mut self.0).poll_write(cx, bytes),
        }
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match Self::RECORDS {
            true => Self::poll_send(&mut self.0, cx, bytes),
            false => Pin::new(&mut self.0).poll_write_vectored(cx, bytes),
        }
    }

    fn is_write_vectored(&self) -> bool {
        Self::RECORDS || self.0.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// A stream over one TCP socket, which can move to another runtime's I/O driver.
pub(crate) trait Rehome {
    /// Whether bytes written to the stream reach the socket unchanged.
    const PLAIN: bool;

    fn socket(&mut self) -> &mut TcpStream;

    /// Registers the socket with the calling runtime's driver through a duplicate descriptor; on an error it stays put.
    fn rehome(&mut self) -> io::Result<()> {
        let socket = self.socket();
        let copy = std::net::TcpStream::from(socket2::SockRef::from(&*socket).try_clone()?);
        copy.set_nonblocking(true)?;
        *socket = TcpStream::from_std(copy)?;
        Ok(())
    }
}

impl Rehome for TcpStream {
    const PLAIN: bool = true;

    fn socket(&mut self) -> &mut TcpStream {
        self
    }
}

impl Rehome for TlsStream<BufReader<TcpStream>> {
    const PLAIN: bool = false;

    fn socket(&mut self) -> &mut TcpStream {
        self.get_mut().0.get_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::poll_fn,
        sync::{Arc, Mutex},
    };

    #[tokio::test]
    async fn a_spent_burst_yields_once_to_the_other_ready_tasks() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let (bulk_order, probe_order) = (order.clone(), order.clone());
        let bulk = tokio::spawn(async move {
            let mut burst = Burst::default();
            burst.moved(BURST_BYTES - 1);
            poll_fn(|cx| burst.poll_turn(cx)).await;
            bulk_order.lock().unwrap().push("under");
            burst.moved(1);
            poll_fn(|cx| burst.poll_turn(cx)).await;
            bulk_order.lock().unwrap().push("spent");
            poll_fn(|cx| burst.poll_turn(cx)).await;
            bulk_order.lock().unwrap().push("fresh");
        });
        let probe = tokio::spawn(async move { probe_order.lock().unwrap().push("probe") });
        bulk.await.unwrap();
        probe.await.unwrap();
        assert_eq!(*order.lock().unwrap(), ["under", "probe", "spent", "fresh"]);
    }

    #[test]
    fn a_rehomed_socket_reads_without_its_first_runtime() {
        use tokio::io::AsyncReadExt;
        let runtime = || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
        };
        let (first, second) = (runtime(), runtime());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let accepted = listener.accept().unwrap().0;
        accepted.set_nonblocking(true).unwrap();
        // Registered with the first runtime's driver, which nothing polls again.
        let mut socket = first
            .block_on(async { TcpStream::from_std(accepted) })
            .unwrap();
        let read = second.block_on(async move {
            socket.rehome().unwrap();
            std::io::Write::write_all(&mut peer, b"ping").unwrap();
            let mut buffer = [0; 4];
            let reading = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                socket.read_exact(&mut buffer),
            );
            reading.await.map(|_| buffer)
        });
        assert_eq!(read.unwrap(), *b"ping");
    }
}

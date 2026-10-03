//! Owned download lanes. Only received bytes contribute to measurement.
use crate::{
    Error,
    failure::Failure,
    net::{Http, url},
    transport::{Retrying, TransferRetry, Transport, cache_buster},
    webtransport::{Session, SessionSlot},
};
use graphite_meter_core::{
    discovery::{ThroughputTarget, ThroughputTransport},
    failure::FailureReason,
    origin::canonical_origin,
    route::Route,
    wire::{MAX_TRANSFER_BYTES, MAX_WEBTRANSPORT_STREAMS},
};
use http::Method;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::{Instant, timeout},
};

/// A stage's download lanes, which count what they receive together.
#[derive(Default)]
pub struct Download {
    bytes: Arc<AtomicU64>,
    retrying: Retrying,
    tasks: JoinSet<Result<(), Error>>,
}

impl Download {
    /// Delay each successive lane before its first request; cancellation covers the delay.
    pub async fn start(
        transport: Arc<Transport>,
        lanes: usize,
        duration: Duration,
        stagger: Duration,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        if !(1..=128).contains(&lanes) || stagger > Duration::from_millis(75) {
            return Err("invalid download lane count or stagger".into());
        }
        let mut owner = Self::default();
        let (ready, mut received) = mpsc::channel(lanes);
        for lane in 0..lanes {
            let (transport, ready) = (transport.clone(), ready.clone());
            owner.spawn(lane, &cancel, move |bytes, retry| async move {
                if lane > 0 && !stagger.is_zero() {
                    tokio::time::sleep(stagger * lane as u32).await;
                }
                receive_http_lane(&transport, lane, &bytes, &ready, duration, retry).await
            });
        }
        drop(ready);
        let readiness = timeout(Duration::from_secs(10), owner.ready(&mut received, lanes, "download"));
        readiness.await.map_err(|_| "download readiness timed out")??;
        Ok(owner)
    }

    /// Start bounded WebTransport lanes using the same received-byte owner as HTTP.
    /// Each connection has one session and at most sixteen readers. A group's session is
    /// dialled again each time it is lost, one dial at a time; observed bytes remain counted.
    pub async fn start_webtransport(
        http: &Http,
        target: &ThroughputTarget,
        lanes: usize,
        duration: Duration,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        if !(1..=128).contains(&lanes) || duration.is_zero() {
            return Err("invalid WebTransport download lanes or duration".into());
        }
        if target.transport != ThroughputTransport::WebTransport {
            return Err("WebTransport download requires a stream target".into());
        }
        let origin = canonical_origin(&target.base_url)?;
        let mut owner = Self::default();
        let (ready, mut received) = mpsc::channel(lanes);
        let lane_cancel = cancel.clone();
        let start = async {
            for first in (0..lanes).step_by(MAX_WEBTRANSPORT_STREAMS) {
                let group = (lanes - first).min(MAX_WEBTRANSPORT_STREAMS);
                let target = format!(
                    "{}?bytes={WT_STREAM_BYTES}&streams={group}",
                    url(&origin, Route::WtDownload, &[])
                );
                let slot = Arc::new(SessionSlot::dial(http, target).await?);
                for lane in first..first + group {
                    let (slot, ready) = (slot.clone(), ready.clone());
                    owner.spawn(lane, &lane_cancel, move |bytes, retry| async move {
                        timeout(duration, receive_webtransport(slot, bytes, ready, retry)).await?
                    });
                }
            }
            drop(ready);
            owner.ready(&mut received, lanes, "WebTransport download").await
        };
        tokio::select! {biased;
            _ = cancel.wait_for(|value| *value) => return Err("download cancelled before readiness".into()),
            result = timeout(Duration::from_secs(10), start) => result??,
        }
        Ok(owner)
    }

    /// Runs lane `lane` until `cancel`, counting into this download's bytes and reporting its retries.
    fn spawn<F>(
        &mut self,
        lane: usize,
        cancel: &watch::Receiver<bool>,
        run: impl FnOnce(Arc<AtomicU64>, TransferRetry) -> F,
    ) where
        F: Future<Output = Result<(), Error>> + Send + 'static,
    {
        let mut cancel = cancel.clone();
        let run = run(self.bytes.clone(), TransferRetry::new(self.retrying.clone(), lane));
        self.tasks.spawn(async move {
            tokio::select! {
                biased;
                _ = cancel.wait_for(|value| *value) => Ok(()),
                result = run => result,
            }
        });
    }

    /// Waits until `lanes` lanes are ready; a lane that ends first reports its cause.
    async fn ready(&mut self, received: &mut mpsc::Receiver<()>, lanes: usize, kind: &str) -> Result<(), Error> {
        for _ in 0..lanes {
            tokio::select! {
                // Lanes drop their sender before their task completes; report the lane's cause.
                value = received.recv() => if value.is_none() {
                    self.tasks.join_next().await.ok_or_else(|| format!("no {kind} lanes"))???;
                    return Err(format!("{kind} ended before readiness").into());
                },
                task = self.tasks.join_next() => {
                    task.ok_or_else(|| format!("no {kind} lanes"))???;
                    return Err(format!("{kind} cancelled before readiness").into());
                }
            }
        }
        Ok(())
    }

    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn retrying(&self) -> Option<Error> {
        self.retrying.failure()
    }

    pub fn health(&mut self) -> Result<(), Error> {
        if let Some(task) = self.tasks.try_join_next() {
            task??;
            return Err(Box::new(Failure::Measurement(FailureReason::ConnectionLost)));
        }
        Ok(())
    }

    pub async fn stop(mut self) {
        self.tasks.shutdown().await;
    }
}

async fn receive_http_lane(
    transport: &Transport,
    lane: usize,
    bytes: &AtomicU64,
    ready: &mpsc::Sender<()>,
    duration: Duration,
    mut retry: TransferRetry,
) -> Result<(), Error> {
    let lane = lane.to_string();
    let requested_bytes = MAX_TRANSFER_BYTES.to_string();
    let mut announced = false;
    loop {
        let started = Instant::now();
        let mut moved = false;
        let attempt = async {
            let query = [("bytes", &*requested_bytes), ("cb", &*cache_buster()), ("lane", &*lane)];
            let mut body = transport
                .receive(Method::GET, Route::Download, &query, MAX_TRANSFER_BYTES, duration)
                .await?;
            if !announced {
                ready.send(()).await.map_err(|_| "download readiness receiver closed")?;
                announced = true;
            }
            // A body that ends, at its length or short of it, ends the attempt (download.go:74).
            while let Some(chunk) = body.chunk().await? {
                bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                moved |= !chunk.is_empty();
            }
            Ok::<(), Error>(())
        };
        retry.ended(attempt.await, started, moved).await?;
    }
}

const WT_STREAM_BYTES: u64 = 64 * 1024 * 1024;

async fn receive_webtransport(
    slot: Arc<SessionSlot>,
    bytes: Arc<AtomicU64>,
    ready: mpsc::Sender<()>,
    retry: TransferRetry,
) -> Result<(), Error> {
    let (bytes, ready, announced) = (&*bytes, &ready, &AtomicBool::new(false));
    slot.lane(retry, move |session| receive_stream(session, bytes, ready, announced))
        .await
}

/// One server stream of the lane, counted as it arrives; the lane is ready at its first bytes.
async fn receive_stream(
    session: Arc<Session>,
    bytes: &AtomicU64,
    ready: &mpsc::Sender<()>,
    announced: &AtomicBool,
) -> (Result<(), Error>, bool) {
    let mut moved = false;
    let result = async {
        let mut stream = session.accept_uni().await?;
        let mut received = 0_u64;
        while let Some(chunk) = stream.read_chunk().await? {
            received = received
                .checked_add(chunk.len() as u64)
                .ok_or("download byte count overflow")?;
            moved |= !chunk.is_empty();
            bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            if !announced.load(Ordering::Relaxed) && !chunk.is_empty() {
                ready.try_send(()).map_err(|_| "download readiness receiver closed")?;
                announced.store(true, Ordering::Relaxed);
            }
            if received > WT_STREAM_BYTES {
                return Err("WebTransport download exceeded its declared byte count".into());
            }
        }
        if received != WT_STREAM_BYTES {
            return Err("WebTransport download ended before its declared byte count".into());
        }
        Ok(())
    }
    .await;
    (result, moved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::h3_endpoint;
    use crate::transport::TRANSFER_RETRY_BACKOFF;
    use graphite_meter_core::discovery::Protocol;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn http_lane_preserves_received_bytes_across_partial_responses() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let mut closed = tokio::time::Instant::now();
            for (attempt, lasts) in [Duration::ZERO, 2 * TRANSFER_RETRY_BACKOFF, Duration::ZERO]
                .into_iter()
                .enumerate()
            {
                let (mut stream, _) = listener.accept().await?;
                match attempt {
                    1 => assert!(closed.elapsed() >= TRANSFER_RETRY_BACKOFF),
                    2 => assert!(closed.elapsed() < TRANSFER_RETRY_BACKOFF),
                    _ => {}
                }
                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).await?;
                assert!(request[..count].starts_with(b"GET /download?bytes=68719476736&cb="));
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 68719476736\r\n\r\n")
                    .await?;
                stream.write_all(&[42; 1024]).await?;
                tokio::time::sleep(lasts).await;
                drop(stream);
                closed = tokio::time::Instant::now();
            }
            Ok::<_, Error>(())
        });
        let transport = Arc::new(Transport::connect(Http::new(false)?, &origin, Protocol::Http1).await?);
        let (_stop, cancelled) = watch::channel(false);
        let mut download = Download::start(transport, 1, Duration::from_secs(5), Duration::ZERO, cancelled).await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while download.bytes() < 3072 {
                download.health()?;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, Error>(())
        })
        .await??;
        download.stop().await;
        server.await??;
        Ok(())
    }

    /// An HTTP/3 body cut short of its length ends the attempt, as quic-go's EOF ends Go's
    /// (download.go:74): here every answer declares 64 GiB and ends after 8 bytes.
    #[tokio::test]
    async fn http3_lane_asks_again_after_a_body_cut_short() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let (endpoint, origin) = h3_endpoint()?;
        let server = tokio::spawn(async move {
            let quic = endpoint.accept().await.ok_or("endpoint closed")?.await?;
            let mut connection = graphite_meter_http3::server::Connection::new(quic, None);
            while let Some(request) = connection.next().await? {
                let (_, stream) = request.resolve().await?;
                let (mut send, _recv) = stream.split();
                let head = http::Response::builder().header(http::header::CONTENT_LENGTH, MAX_TRANSFER_BYTES);
                send.send_response(head.body(())?).await?;
                send.send_data(bytes::Bytes::from_static(b"progress")).await?;
                send.finish().await?;
            }
            Ok::<_, Error>(())
        });
        let transport = Arc::new(Transport::connect(Http::new(true)?, &origin, Protocol::Http3).await?);
        let (_stop, cancelled) = watch::channel(false);
        let mut download = Download::start(transport, 1, Duration::from_secs(5), Duration::ZERO, cancelled).await?;
        let asked_again = timeout(Duration::from_secs(5), async {
            while download.bytes() < 24 {
                download.health()?;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok::<_, Error>(())
        })
        .await;
        download.stop().await;
        server.abort();
        asked_again?
    }

    #[tokio::test]
    async fn webtransport_download_counts_payload_without_stream_headers() -> Result<(), Error> {
        use graphite_meter_http3::{server::Connection, webtransport::Session as PeerSession};
        timeout(Duration::from_secs(5), async {
            let _ = crate::crypto::provider().install_default();
            let (endpoint, origin) = h3_endpoint()?;
            let mut servers = JoinSet::new();
            servers.spawn(async move {
                let quic = endpoint.accept().await.ok_or("endpoint closed")?.await?;
                let mut connection = Connection::new(quic, None);
                let (_, stream) = connection.next().await?.ok_or("missing CONNECT")?.resolve().await?;
                let payload = async {
                    let session = PeerSession::accept(stream, http::HeaderMap::new()).await?;
                    let mut lane = session.open_uni().await?;
                    lane.write_all(b"progress").await?;
                    // Leave the stream open so cancellation, rather than EOF, ends the lane.
                    std::future::pending::<Result<(), Error>>().await
                };
                let driver = async {
                    while connection.next().await?.is_some() {}
                    Ok::<_, Error>(())
                };
                tokio::try_join!(payload, driver)?;
                Ok::<_, Error>(())
            });
            let (_stop, cancelled) = watch::channel(false);
            let download = Download::start_webtransport(
                &Http::new(true)?,
                &ThroughputTarget {
                    base_url: origin,
                    protocol: Protocol::Http3,
                    transport: ThroughputTransport::WebTransport,
                },
                1,
                Duration::from_secs(30),
                cancelled,
            )
            .await?;
            let measured = download.bytes();
            download.stop().await;
            servers.shutdown().await;
            assert_eq!(measured, 8, "WebTransport counted stream framing as payload");
            Ok::<_, Error>(())
        })
        .await?
    }

    /// A refusal ends the lane immediately, without another request or the retry window.
    #[tokio::test]
    async fn http_lane_stops_at_a_refusal() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let served = Arc::new(AtomicU64::new(0));
        let seen = served.clone();
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let _ = stream.read(&mut [0_u8; 4096]).await?;
                seen.fetch_add(1, Ordering::SeqCst);
                stream
                    .write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await?;
            }
            Ok::<_, Error>(())
        });
        let transport = Transport::connect(Http::new(false)?, &origin, Protocol::Http1).await?;
        let (ready, _announced) = mpsc::channel(1);
        let bytes = AtomicU64::new(0);
        let retry = TransferRetry::new(Retrying::default(), 0);
        let started = Instant::now();
        let lane = receive_http_lane(&transport, 0, &bytes, &ready, Duration::from_secs(5), retry);
        let error = timeout(Duration::from_secs(5), lane).await?.unwrap_err();
        server.abort();
        assert_eq!(
            crate::failure::reason(error.as_ref(), false),
            FailureReason::ProtocolError
        );
        assert_eq!(served.load(Ordering::SeqCst), 1);
        assert!(started.elapsed() < Duration::from_secs(2));
        Ok(())
    }
}

//! Owned download lanes. Only received bytes contribute to measurement.
use crate::{
    Error,
    failure::Failure,
    net::{Http, url},
    transport::{Retrying, TransferRetry, Transport, cache_buster},
    webtransport::SessionSlot,
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

/// Where a stage's download lanes read.
pub enum Source<'a> {
    /// HTTP requests on a transport, each lane starting the stagger after the one before.
    Http(Arc<Transport>, Duration),
    /// WebTransport sessions, each on a connection of its own with at most sixteen readers.
    WebTransport(&'a Http, &'a ThroughputTarget),
}

impl Download {
    /// `lanes` lanes from `source`, ready once every lane is; cancellation covers their start.
    pub async fn start(
        source: Source<'_>,
        lanes: usize,
        duration: Duration,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        if !(1..=128).contains(&lanes) || duration.is_zero() {
            return Err("invalid download lane count or duration".into());
        }
        let (mut owner, (ready, mut received)) = (Self::default(), mpsc::channel(lanes));
        let lane_cancel = cancel.clone();
        let start = async {
            match source {
                Source::Http(_, stagger) if stagger > Duration::from_millis(75) => {
                    return Err("invalid download lane stagger".into());
                }
                Source::Http(transport, stagger) => {
                    for lane in 0..lanes {
                        let (transport, ready) = (transport.clone(), ready.clone());
                        owner.spawn(&transport.lane_home(), lane, &lane_cancel, move |bytes, retry| async move {
                            if lane > 0 && !stagger.is_zero() {
                                tokio::time::sleep(stagger * lane as u32).await;
                            }
                            receive_http_lane(&transport, lane, &bytes, &ready, duration, retry).await
                        });
                    }
                }
                Source::WebTransport(_, target) if target.transport != ThroughputTransport::WebTransport => {
                    return Err("WebTransport download requires a stream target".into());
                }
                // A group's session is dialled again each time it is lost, one dial at a time.
                Source::WebTransport(http, target) => {
                    let origin = canonical_origin(&target.base_url)?;
                    for first in (0..lanes).step_by(MAX_WEBTRANSPORT_STREAMS) {
                        let group = (lanes - first).min(MAX_WEBTRANSPORT_STREAMS);
                        let query = [("bytes", &*WT_STREAM_BYTES.to_string()), ("streams", &*group.to_string())];
                        let slot = Arc::new(SessionSlot::dial(http, url(&origin, Route::WtDownload, &query)).await?);
                        for lane in first..first + group {
                            let (home, slot, ready) = (slot.home.clone(), slot.clone(), ready.clone());
                            owner.spawn(&home, lane, &lane_cancel, move |bytes, retry| async move {
                                timeout(duration, receive_webtransport(slot, bytes, ready, retry)).await?
                            });
                        }
                    }
                }
            }
            drop(ready);
            owner.ready(&mut received, lanes).await
        };
        let timed_out = || std::io::Error::new(std::io::ErrorKind::TimedOut, "download readiness timed out");
        tokio::select! {
            biased;
            _ = cancel.wait_for(|value| *value) => return Err("download cancelled before readiness".into()),
            result = timeout(Duration::from_secs(10), start) => result.map_err(|_| timed_out())??,
        }
        Ok(owner)
    }

    /// Runs lane `lane` until `cancel`, counting into this download's bytes and reporting its retries.
    fn spawn<F>(
        &mut self,
        home: &tokio::runtime::Handle,
        lane: usize,
        cancel: &watch::Receiver<bool>,
        run: impl FnOnce(Arc<AtomicU64>, TransferRetry) -> F,
    ) where
        F: Future<Output = Result<(), Error>> + Send + 'static,
    {
        let mut cancel = cancel.clone();
        let run = run(self.bytes.clone(), TransferRetry::new(self.retrying.clone(), lane));
        let lane = async move {
            tokio::select! {
                biased;
                _ = cancel.wait_for(|value| *value) => Ok(()),
                result = run => result,
            }
        };
        self.tasks.spawn_on(lane, home);
    }

    /// Waits until `lanes` lanes are ready; a lane that ends first reports its cause.
    async fn ready(&mut self, received: &mut mpsc::Receiver<()>, lanes: usize) -> Result<(), Error> {
        for _ in 0..lanes {
            tokio::select! {
                // Lanes drop their sender before their task completes; report the lane's cause.
                value = received.recv() => if value.is_none() {
                    self.tasks.join_next().await.ok_or("no download lanes")???;
                    return Err("download ended before readiness".into());
                },
                task = self.tasks.join_next() => {
                    task.ok_or("no download lanes")???;
                    return Err("download cancelled before readiness".into());
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
            let body = transport.receive(Method::GET, Route::Download, &query, MAX_TRANSFER_BYTES, duration);
            let mut body = body.await?;
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

/// Server streams on the lane's session, counted as they arrive; the lane is ready at its first bytes.
async fn receive_webtransport(
    slot: Arc<SessionSlot>,
    bytes: Arc<AtomicU64>,
    ready: mpsc::Sender<()>,
    retry: TransferRetry,
) -> Result<(), Error> {
    let (bytes, ready, announced) = (&*bytes, &ready, &AtomicBool::new(false));
    slot.lane(retry, move |session| async move {
        let mut moved = false;
        let result = async {
            let mut stream = session.accept_uni().await?;
            let mut received = 0_u64;
            while let Some(chunk) = stream.read_chunk().await? {
                let size = chunk.len() as u64;
                received = received.checked_add(size).ok_or("download byte count overflow")?;
                moved |= size > 0;
                bytes.fetch_add(size, Ordering::Relaxed);
                if !announced.load(Ordering::Relaxed) && size > 0 {
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
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::h3_endpoint;
    use crate::transport::TRANSFER_RETRY_BACKOFF;
    use graphite_meter_core::discovery::Protocol;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One lane from `origin` over `protocol`, until it received `bytes` or failed.
    async fn receive(origin: &str, protocol: Protocol, bytes: u64) -> Result<(), Error> {
        let http = Http::new(protocol == Protocol::Http3)?;
        let transport = Arc::new(Transport::connect(http, origin, protocol).await?);
        let (_stop, cancelled) = watch::channel(false);
        let source = Source::Http(transport, Duration::ZERO);
        let mut download = Download::start(source, 1, Duration::from_secs(5), cancelled).await?;
        let received = timeout(Duration::from_secs(5), async {
            while download.bytes() < bytes {
                download.health()?;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok::<_, Error>(())
        })
        .await;
        download.stop().await;
        received?
    }

    #[tokio::test]
    async fn http_lane_preserves_received_bytes_across_partial_responses() -> Result<(), Error> {
        let (listener, origin) = crate::fixtures::listener().await?;
        let server = tokio::spawn(async move {
            let mut closed = tokio::time::Instant::now();
            for (attempt, lasts) in [(0, Duration::ZERO), (1, 2 * TRANSFER_RETRY_BACKOFF), (2, Duration::ZERO)] {
                let (mut stream, _) = listener.accept().await?;
                match attempt {
                    1 => assert!(closed.elapsed() >= TRANSFER_RETRY_BACKOFF),
                    2 => assert!(closed.elapsed() < TRANSFER_RETRY_BACKOFF),
                    _ => {}
                }
                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).await?;
                assert!(request[..count].starts_with(b"GET /download?bytes=68719476736&cb="));
                let head = b"HTTP/1.1 200 OK\r\nContent-Length: 68719476736\r\n\r\n";
                stream.write_all(head).await?;
                stream.write_all(&[42; 1024]).await?;
                tokio::time::sleep(lasts).await;
                drop(stream);
                closed = tokio::time::Instant::now();
            }
            Ok::<_, Error>(())
        });
        receive(&origin, Protocol::Http1, 3072).await?;
        server.await??;
        Ok(())
    }

    /// An HTTP/3 body cut short of its length ends the attempt, as quic-go's EOF ends Go's (download.go:74).
    #[tokio::test]
    async fn http3_lane_asks_again_after_a_body_cut_short() -> Result<(), Error> {
        let (endpoint, origin) = h3_endpoint()?;
        let server = tokio::spawn(async move {
            let mut connection = crate::fixtures::h3_connection(&endpoint).await?;
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
        let asked_again = receive(&origin, Protocol::Http3, 24).await;
        server.abort();
        asked_again
    }

    #[tokio::test]
    async fn webtransport_download_counts_payload_without_stream_headers() -> Result<(), Error> {
        timeout(Duration::from_secs(5), async {
            let (endpoint, base_url) = h3_endpoint()?;
            let server = tokio::spawn(crate::fixtures::webtransport_peer(endpoint, |session| async move {
                let mut lane = session.open_uni().await?;
                lane.write_all(b"progress").await?;
                // Leave the stream open so cancellation, rather than EOF, ends the lane.
                std::future::pending().await
            }));
            let (_stop, cancelled) = watch::channel(false);
            let (protocol, transport) = (Protocol::Http3, ThroughputTransport::WebTransport);
            let target = ThroughputTarget { base_url, protocol, transport };
            let http = Http::new(true)?;
            let source = Source::WebTransport(&http, &target);
            let download = Download::start(source, 1, Duration::from_secs(30), cancelled).await?;
            let measured = download.bytes();
            download.stop().await;
            server.abort();
            assert_eq!(measured, 8, "WebTransport counted stream framing as payload");
            Ok::<_, Error>(())
        })
        .await?
    }

    /// A refusal ends the lane immediately, without another request or the retry window.
    #[tokio::test]
    async fn http_lane_stops_at_a_refusal() -> Result<(), Error> {
        let served = Arc::new(AtomicU64::new(0));
        let seen = served.clone();
        let origin = crate::fixtures::peer(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            Some("HTTP/1.1 410 Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into())
        });
        let started = Instant::now();
        let error = receive(&origin.await?, Protocol::Http1, 1).await.unwrap_err();
        assert_eq!(crate::failure::reason(error.as_ref(), false), FailureReason::ProtocolError);
        assert_eq!(served.load(Ordering::SeqCst), 1);
        assert!(started.elapsed() < Duration::from_secs(2));
        Ok(())
    }
}

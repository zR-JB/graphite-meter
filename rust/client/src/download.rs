//! Owned download lanes. Only received bytes contribute to measurement.
use crate::{
    Error,
    failure::MeasurementFailure,
    net::Http,
    transport::{Retrying, TransferRetry, Transport},
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
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::{Instant, timeout},
};

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
        let bytes = Arc::new(AtomicU64::new(0));
        let mut owner = Self {
            bytes,
            retrying: Retrying::default(),
            tasks: JoinSet::new(),
        };
        let (ready, mut received) = mpsc::channel(lanes);
        for lane in 0..lanes {
            let transport = transport.clone();
            let bytes = owner.bytes.clone();
            let ready = ready.clone();
            let mut cancel = cancel.clone();
            let retry = TransferRetry::new(owner.retrying.clone(), lane);
            owner.tasks.spawn(async move {
                let transfer = async {
                    if lane > 0 && !stagger.is_zero() {
                        tokio::time::sleep(stagger * lane as u32).await;
                    }
                    receive_http_lane(&transport, lane, &bytes, &ready, duration, retry).await
                };
                tokio::select! {
                    biased;
                    _ = cancel.wait_for(|value| *value) => Ok(()),
                    result = transfer => result,
                }
            });
        }
        drop(ready);
        let mut ready_lanes = 0;
        let readiness = async {
            for _ in 0..lanes {
                tokio::select! {
                    // Lanes drop their sender before their task completes; report the lane's cause.
                    value = received.recv() => if value.is_none() {
                        owner.tasks.join_next().await.ok_or("no download lanes")???;
                        return Err("download ended before readiness".into());
                    },
                    task = owner.tasks.join_next() => {
                        task.ok_or("no download lanes")???;
                        return Err::<(), Error>("download cancelled before readiness".into());
                    }
                }
                ready_lanes += 1;
            }
            Ok(())
        };
        match timeout(Duration::from_secs(10), readiness).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(format!(
                    "download readiness timed out: {ready_lanes}/{lanes} lanes received response headers"
                )
                .into());
            }
        }
        Ok(owner)
    }

    /// Start bounded WebTransport lanes using the same received-byte owner as HTTP.
    /// Each connection has one session and at most sixteen readers. Lost
    /// connections are replaced once per group; observed bytes remain counted.
    pub async fn start_webtransport(
        http: &Http,
        target: &ThroughputTarget,
        lanes: usize,
        duration: Duration,
        insecure: bool,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Self, Error> {
        if !(1..=128).contains(&lanes) || duration.is_zero() {
            return Err("invalid WebTransport download lanes or duration".into());
        }
        if target.transport != ThroughputTransport::WebTransport {
            return Err("WebTransport download requires a stream target".into());
        }
        let origin = canonical_origin(&target.base_url)?;
        let mut owner = Self {
            bytes: Arc::new(AtomicU64::new(0)),
            retrying: Retrying::default(),
            tasks: JoinSet::new(),
        };
        let (ready, mut received) = mpsc::channel(lanes);
        let lane_cancel = cancel.clone();
        let start = async {
            for first in (0..lanes).step_by(MAX_WEBTRANSPORT_STREAMS) {
                let group = (lanes - first).min(MAX_WEBTRANSPORT_STREAMS);
                let target = format!("{origin}/wt/download?bytes={WT_STREAM_BYTES}&streams={group}");
                let slot = Arc::new(SessionSlot::dial(http, target, insecure).await?);
                for lane in first..first + group {
                    let slot = slot.clone();
                    let bytes = owner.bytes.clone();
                    let ready = ready.clone();
                    let mut cancel = lane_cancel.clone();
                    let retry = TransferRetry::new(owner.retrying.clone(), lane);
                    owner.tasks.spawn(async move {
                        tokio::select! {biased;
                            _ = cancel.wait_for(|value| *value) => Ok(()),
                            result = timeout(duration, receive_webtransport(slot, bytes, ready, retry)) => result?,
                        }
                    });
                }
            }
            drop(ready);
            for _ in 0..lanes {
                tokio::select! {
                    value = received.recv() => if value.is_none() {
                        owner.tasks.join_next().await.ok_or("no WebTransport download lanes")???;
                        return Err("WebTransport download ended before readiness".into());
                    },
                    task = owner.tasks.join_next() => {
                        task.ok_or("no WebTransport download lanes")???;
                        return Err::<(), Error>("WebTransport download cancelled before readiness".into());
                    }
                }
            }
            Ok(())
        };
        tokio::select! {biased;
            _ = cancel.wait_for(|value| *value) => return Err("download cancelled before readiness".into()),
            result = timeout(Duration::from_secs(10), start) => result??,
        }
        Ok(owner)
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
            return Err(Box::new(MeasurementFailure(FailureReason::ConnectionLost)));
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
            let mut body = transport
                .receive(
                    Method::GET,
                    Route::Download,
                    &[("bytes", &requested_bytes), ("lane", &lane)],
                    MAX_TRANSFER_BYTES,
                    duration,
                )
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
    mut retry: TransferRetry,
) -> Result<(), Error> {
    let mut announced = false;
    loop {
        let session = slot.current().await;
        let started = Instant::now();
        let mut moved = false;
        let result = receive_webtransport_chunk(&session, &bytes, &ready, &mut announced, &mut moved).await;
        retry.ended(result, started, moved).await?;
        if session.is_closed() {
            let started = Instant::now();
            if let Err(error) = slot.reconnect(&session).await {
                retry.ended(Err(error), started, false).await?;
            }
        }
    }
}

async fn receive_webtransport_chunk(
    session: &Session,
    bytes: &AtomicU64,
    ready: &mpsc::Sender<()>,
    announced: &mut bool,
    moved: &mut bool,
) -> Result<(), Error> {
    let mut stream = session.accept_uni().await?;
    let mut received = 0_u64;
    while let Some(chunk) = stream.read_chunk().await? {
        received = received
            .checked_add(chunk.len() as u64)
            .ok_or("download byte count overflow")?;
        *moved |= !chunk.is_empty();
        record_webtransport(bytes, ready, announced, chunk.len())?;
        if received > WT_STREAM_BYTES {
            return Err("WebTransport download exceeded its declared byte count".into());
        }
    }
    if received != WT_STREAM_BYTES {
        return Err("WebTransport download ended before its declared byte count".into());
    }
    Ok(())
}

fn record_webtransport(
    bytes: &AtomicU64,
    ready: &mpsc::Sender<()>,
    announced: &mut bool,
    count: usize,
) -> Result<(), Error> {
    bytes.fetch_add(count as u64, Ordering::Relaxed);
    if !*announced && count > 0 {
        ready.try_send(()).map_err(|_| "download readiness receiver closed")?;
        *announced = true;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
                assert!(request[..count].starts_with(b"GET /download?"));
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
        let transport = Arc::new(Transport::connect(Http::new(false)?, &origin, Protocol::Http1, false).await?);
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
        let transport = Arc::new(Transport::connect(Http::new(true)?, &origin, Protocol::Http3, true).await?);
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

    /// A WebTransport session is dialled again as Go's restore dials it (webtransport.go:169-183),
    /// here once a busy answer's Retry-After has passed.
    #[tokio::test]
    async fn a_busy_webtransport_session_is_dialled_again() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let (endpoint, origin) = h3_endpoint()?;
        let dials = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = dials.clone();
        let server = tokio::spawn(async move {
            let mut refused = Vec::new();
            loop {
                let quic = endpoint.accept().await.ok_or("endpoint closed")?.await?;
                let mut connection = graphite_meter_http3::server::Connection::new(quic, None);
                let (_, stream) = connection.next().await?.ok_or("no CONNECT")?.resolve().await?;
                seen.lock().unwrap().push(Instant::now());
                if !refused.is_empty() {
                    // The connection's driver writes the session's answer.
                    let accept = graphite_meter_http3::webtransport::Session::accept(stream, http::HeaderMap::new());
                    let _ = tokio::join!(accept, async { while let Ok(Some(_)) = connection.next().await {} });
                    return Ok::<_, Error>(());
                }
                let (mut send, _recv) = stream.split();
                let busy = http::Response::builder()
                    .status(503)
                    .header(http::header::RETRY_AFTER, "1");
                send.send_response(busy.body(())?).await?;
                send.finish().await?;
                refused.push(connection);
            }
        });
        let target = format!("{origin}/wt/download?bytes=0");
        let slot = SessionSlot::dial(&Http::new(true)?, target, true).await;
        server.abort();
        slot?.close().await;
        let dials = dials.lock().unwrap();
        assert!(dials[1] - dials[0] >= Duration::from_secs(1), "{dials:?}");
        Ok(())
    }

    /// A local HTTP/3 endpoint whose certificate the client takes only when insecure.
    fn h3_endpoint() -> Result<(quinn::Endpoint, String), Error> {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
        let (certificate, key) = crate::test_identity::generate_identity("localhost")?;
        let mut tls = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(
            vec![CertificateDer::from_pem_slice(certificate.as_bytes())?],
            PrivateKeyDer::from_pem_slice(key.as_bytes())?,
        )?;
        tls.alpn_protocols = vec![b"h3".to_vec()];
        let tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
        let endpoint =
            quinn::Endpoint::server(quinn::ServerConfig::with_crypto(Arc::new(tls)), "127.0.0.1:0".parse()?)?;
        let origin = format!("https://{}", endpoint.local_addr()?);
        Ok((endpoint, origin))
    }

    /// Go's TestLanePersistence (transfer_test.go:87-151): only a refusal ends a lane at once; a
    /// refused connection and an empty answer are retried for 2 s, 500 ms apart.
    #[tokio::test]
    async fn http_lane_retries_all_but_a_refusal_like_go() -> Result<(), Error> {
        use graphite_meter_core::failure::FailureReason::{ConnectionLost, ProtocolError, ServerBusy, Timeout};
        let _ = crate::crypto::provider().install_default();
        let mut cases = JoinSet::new();
        for (answer, reason, requests) in [
            (Some("429 Too Many Requests"), ServerBusy, 4),
            (Some("410 Gone"), ProtocolError, 1),
            (Some("200 OK"), Timeout, 5),
            (None, ConnectionLost, 0),
        ] {
            cases.spawn(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await?;
                let origin = format!("http://{}", listener.local_addr()?);
                let served = Arc::new(AtomicU64::new(0));
                // Without an answer the port closes, and every dial is refused.
                let server = answer.map(|answer| tokio::spawn(serve(listener, answer, served.clone())));
                let transport = Transport::connect(Http::new(false)?, &origin, Protocol::Http1, false).await?;
                let (ready, _announced) = mpsc::channel(1);
                let bytes = AtomicU64::new(0);
                let retry = TransferRetry::new(Retrying::default(), 0);
                let started = Instant::now();
                let lane = receive_http_lane(&transport, 0, &bytes, &ready, Duration::from_secs(5), retry);
                let error = timeout(Duration::from_secs(5), lane).await?.unwrap_err();
                let lasted = started.elapsed();
                if let Some(server) = server {
                    server.abort();
                }
                assert_eq!(
                    crate::failure::reason(error.as_ref(), false),
                    reason,
                    "{answer:?}: {error}"
                );
                assert_eq!(served.load(Ordering::SeqCst), requests, "{answer:?}");
                assert_eq!(
                    lasted >= Duration::from_secs(2),
                    requests != 1,
                    "{answer:?} after {lasted:?}"
                );
                Ok::<_, Error>(())
            });
        }
        while let Some(case) = cases.join_next().await {
            case??;
        }
        Ok(())
    }

    /// Answers each request with an empty `status` response on a connection of its own.
    async fn serve(listener: TcpListener, status: &str, served: Arc<AtomicU64>) -> Result<(), Error> {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let _ = stream.read(&mut [0_u8; 4096]).await?;
            let head = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            stream.write_all(head.as_bytes()).await?;
            served.fetch_add(1, Ordering::SeqCst);
        }
    }
}

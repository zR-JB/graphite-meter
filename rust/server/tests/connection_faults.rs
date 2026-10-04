#[path = "support/native.rs"]
mod native;

#[path = "support/quic.rs"]
mod quic;

#[path = "../../test_link.rs"]
mod test_link;

use bytes::Bytes;
use graphite_meter_http3::{self as http3, Code, webtransport::Session};
use graphite_meter_server::config::{Config, NativeKind};
use graphite_meter_server::http::HttpServer;
use http::{Request, Version};
use quic::{QuicServer, TestError, body, json, requests};
use rustls::{
    ClientConfig, ServerConfig,
    pki_types::ServerName,
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

struct Tls {
    server: ServerConfig,
    client: ClientConfig,
}
impl Tls {
    fn new() -> Self {
        let (server, client) = quic::test_tls::configs("localhost", &[&rustls::version::TLS13], &[b"h2"]).unwrap();
        Self { server, client }
    }
    fn server(&self, resolver: Arc<dyn ResolvesServerCert>) -> ServerConfig {
        let mut server = self.server.clone();
        server.cert_resolver = resolver;
        server
    }
    fn client(&self) -> TlsConnector {
        TlsConnector::from(Arc::new(self.client.clone()))
    }
}

#[derive(Debug)]
struct PanicOnce(AtomicBool, Arc<dyn ResolvesServerCert>);
impl ResolvesServerCert for PanicOnce {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        if !self.0.swap(true, Ordering::SeqCst) {
            panic!("injected connection fault");
        }
        self.1.resolve(hello)
    }
}

async fn h2_client(tls: &Tls, address: SocketAddr) -> Result<h2::client::SendRequest<Bytes>, TestError> {
    let stream = tls
        .client()
        .connect(ServerName::try_from("localhost")?, TcpStream::connect(address).await?)
        .await?;
    let (client, connection) = h2::client::handshake(stream).await?;
    tokio::spawn(connection);
    Ok(client)
}

fn request(method: &str, path: &str) -> Request<()> {
    Request::builder()
        .method(method)
        .uri(format!("https://localhost{path}"))
        .version(Version::HTTP_2)
        .body(())
        .unwrap()
}

async fn serve_h2(tls: ServerConfig, config: Config) -> native::NativeServer {
    native::serve(
        Arc::new(HttpServer::new(config.validated().unwrap()).unwrap()),
        NativeKind::H2,
        Some(Arc::new(tls)),
    )
    .await
}

#[tokio::test]
async fn one_connection_panic_leaves_the_listener_serving() -> Result<(), TestError> {
    let tls = Tls::new();
    let server = serve_h2(
        tls.server(Arc::new(PanicOnce(AtomicBool::new(false), tls.server.cert_resolver.clone()))),
        Config { max_connections_per_client: 1, ..Config::default() },
    )
    .await;
    let address = server.address;
    assert!(h2_client(&tls, address).await.is_err(), "first handshake must fail");
    let mut client = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(client) = h2_client(&tls, address).await {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| "listener stopped after one connection panicked")?;
    client = client.ready().await?;
    let (response, _) = client.send_request(request("GET", "/probe"), true)?;
    assert_eq!(response.await?.status(), 200);
    assert!(!server.task.is_finished(), "listener ended");
    server.shutdown().await;
    Ok(())
}

async fn h2_body(response: http::Response<h2::RecvStream>) -> Result<Vec<u8>, TestError> {
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk?;
        body.flow_control().release_capacity(chunk.len())?;
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[tokio::test]
async fn h2_upload_is_not_window_bound_on_a_delayed_link() -> Result<(), TestError> {
    let tls = Tls::new();
    let server = serve_h2(
        tls.server(tls.server.cert_resolver.clone()),
        Config {
            max_operation_duration: Duration::from_secs(10),
            ..Config::default()
        },
    )
    .await;
    let address = server.address;
    let one_way = Duration::from_millis(20);
    let link = test_link::Link::tcp(address, one_way).await?;
    let mut client = h2_client(&tls, link.address).await?;
    client = client.ready().await?;
    let (session, _) = client.send_request(request("POST", "/upload/session"), true)?;
    let session: serde_json::Value = serde_json::from_slice(&h2_body(session.await?).await?)?;
    let id = session["uploadId"].as_str().ok_or("upload id")?.to_owned();

    let mut remaining = Bytes::from(vec![7_u8; 2 * 1024 * 1024]);
    client = client.ready().await?;
    let started = tokio::time::Instant::now();
    let (response, mut upload) = client.send_request(request("POST", &format!("/upload?id={id}")), false)?;
    while !remaining.is_empty() {
        upload.reserve_capacity(remaining.len().min(256 * 1024));
        let capacity = std::future::poll_fn(|cx| upload.poll_capacity(cx))
            .await
            .ok_or("upload stream closed")??;
        let chunk = remaining.split_to(capacity.min(remaining.len()));
        upload.send_data(chunk, remaining.is_empty())?;
    }
    let response = response.await?;
    let elapsed = started.elapsed();
    let reply: serde_json::Value = serde_json::from_slice(&h2_body(response).await?)?;
    assert_eq!(reply["bytes"], 2 * 1024 * 1024);
    let round_trips = elapsed.as_secs_f64() / (2.0 * one_way.as_secs_f64());
    eprintln!("h2 2 MiB upload: {elapsed:?} = {round_trips:.1} RTT");
    assert!(round_trips >= 1.0, "the link did not delay the upload");
    assert!(round_trips < 10.0, "window-bound upload: {round_trips:.1} RTT");
    server.shutdown().await;
    Ok(())
}

async fn upload_id(requests: &http3::client::SendRequest) -> Result<String, TestError> {
    let session = json(requests, "POST", "/upload/session").await?;
    Ok(session["uploadId"].as_str().ok_or("upload id")?.to_owned())
}

#[tokio::test]
async fn h3_upload_is_not_floor_window_bound_on_a_delayed_link() -> Result<(), TestError> {
    let server = quic::serve(Config {
        max_operation_duration: Duration::from_secs(10),
        ..Config::default()
    })?;
    let link = test_link::Link::udp(server.address, Duration::from_millis(20)).await?;
    let quic = client(&server, true)?.connect(link.address, "localhost")?.await?;
    let (driving, requests) = requests(quic.clone());
    let id = upload_id(&requests).await?;

    let size = 4 * 1024 * 1024;
    let granted = quic.stats().frame_rx.max_data;
    let reply = body(&requests, "POST", &format!("/upload?id={id}"), vec![7_u8; size]).await?;
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&reply)?["bytes"], size);
    let updates = quic.stats().frame_rx.max_data - granted;
    let floor_bound = (size - 64 * 1024) as u64 / (64 * 1024);
    assert!(
        updates < floor_bound,
        "{updates} MAX_DATA round trips; each raises a 64 KiB floor window by 64 KiB at most"
    );
    quic.close(0_u32.into(), b"done");
    driving.abort();
    server.stop().await
}

fn client(server: &QuicServer, reliable_reset: bool) -> Result<noq::Endpoint, TestError> {
    let mut endpoint_config = noq::EndpointConfig::default();
    endpoint_config.reliable_stream_reset(reliable_reset);
    let endpoint = noq::Endpoint::new(
        endpoint_config,
        None,
        graphite_meter_core::socket::udp_socket("127.0.0.1:0".parse()?)?.0,
        noq::default_runtime().unwrap(),
    )?;
    endpoint.set_default_client_config(server.client.clone());
    Ok(endpoint)
}

#[tokio::test]
async fn quic_retry_under_load_or_for_a_connected_source() -> Result<(), TestError> {
    let server = quic::serve(Config {
        max_connections: 12,
        max_connections_per_client: 8,
        ..Config::default()
    })?;
    let client = client(&server, true)?;
    let mut held = Vec::new();
    // Below a quarter of the limit only a source that already holds a QUIC connection answers Retry, as in Go;
    // from it, every unvalidated source does.
    for (load, source, retry) in [(0, 2, false), (1, 3, false), (2, 2, true), (3, 4, true)] {
        let link = test_link::Link::udp_from([127, 0, 0, source].into(), server.address, Duration::ZERO).await?;
        let connection = client.connect(link.address, "localhost")?.await?;
        assert_eq!(link.retries() > 0, retry, "load {load} from 127.0.0.{source}");
        held.push((link, connection));
    }
    server.stop().await
}

/// The UDP link's delay and blackhole, which the QUIC tests rely on and nothing else shows. The TCP link's delay
/// shows in the HTTP/2 upload over it.
#[tokio::test]
async fn udp_delay_and_blackhole() {
    let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = server.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buffer = [0; 1500];
        while let Ok((count, from)) = server.recv_from(&mut buffer).await {
            let _ = server.send_to(&buffer[..count], from).await;
        }
    });
    let link = test_link::Link::udp(target, Duration::from_millis(20)).await.unwrap();
    let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.connect(link.address).await.unwrap();
    let started = tokio::time::Instant::now();
    client.send(b"ping").await.unwrap();
    let mut reply = [0; 16];
    client.recv(&mut reply).await.unwrap();
    let rtt = started.elapsed();
    let delayed = rtt >= Duration::from_millis(40) && rtt < Duration::from_secs(2);
    assert!(delayed, "{rtt:?}");
    link.inject(test_link::Fault::Stall);
    client.send(b"ping").await.unwrap();
    let blackholed = tokio::time::timeout(Duration::from_millis(200), client.recv(&mut reply)).await;
    assert!(blackholed.is_err());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "release-only delayed-link throughput gate"]
async fn quic_downloads_exceed_the_old_window_limit() -> Result<(), TestError> {
    if cfg!(debug_assertions) {
        return Err("run this gate with --release".into());
    }
    tokio::time::timeout(Duration::from_secs(60), async {
        for webtransport in [false, true] {
            let baseline = download_rate(webtransport, Duration::ZERO).await?;
            if baseline < 671.0 {
                eprintln!("inconclusive: webtransport={webtransport}, loopback below 671 Mbit/s");
                continue;
            }
            let delayed = download_rate(webtransport, Duration::from_millis(50)).await?;
            assert!(
                delayed > 419.0,
                "webtransport={webtransport}: delayed throughput below 419 Mbit/s; see raw samples"
            );
        }
        Ok::<_, TestError>(())
    })
    .await?
}

async fn download_rate(webtransport: bool, one_way: Duration) -> Result<f64, TestError> {
    let server = quic::serve(Config::default())?;
    let link = test_link::Link::udp(server.address, one_way).await?;
    let target = if one_way.is_zero() { server.address } else { link.address };
    let mut config = server.client.clone();
    let mut transport = noq::TransportConfig::default();
    transport.stream_receive_window((64_u32 << 20).into());
    transport.receive_window((64_u32 << 20).into());
    config.transport_config(Arc::new(transport));
    let quic = client(&server, true)?
        .connect_with(config, target, "localhost")?
        .await?;
    let (driving, requests) = requests(quic.clone());
    let path = if webtransport { "wt/download" } else { "download" };
    let request = Request::get(format!("https://localhost/{path}?bytes=4294967296")).body(())?;
    let (mut lane, mut body, _session) = if webtransport {
        let (session, _) = Session::connect(&requests, request)
            .await?
            .map_err(|refused| format!("{refused:?}"))?;
        (Some(session.accept_uni().await.ok_or("missing download stream")?), None, Some(session))
    } else {
        let (mut send, mut recv) = requests.send_request(request).await?.split();
        send.finish().await?;
        assert_eq!(recv.response().await?.status(), 200);
        (None, Some(recv), None)
    };
    let started = tokio::time::Instant::now();
    let mut marks = Vec::new();
    let mut total = 0_u64;
    let mut next = Duration::from_millis(500);
    while started.elapsed() < Duration::from_secs(4) {
        let chunk = match (&mut lane, &mut body) {
            (Some(lane), _) => lane.read_chunk().await?,
            (_, Some(body)) => body.data().await?,
            _ => None,
        };
        let Some(chunk) = chunk else { break };
        total += chunk.len() as u64;
        if started.elapsed() >= next {
            marks.push((started.elapsed(), total));
            next = started.elapsed() + Duration::from_millis(500);
        }
    }
    let mut rates = Vec::new();
    for pair in marks.windows(2) {
        let ((before, a), (at, b)) = (pair[0], pair[1]);
        let rate = (b - a) as f64 * 8.0 / (at - before).as_secs_f64() / 1e6;
        eprintln!("webtransport={webtransport} one_way={one_way:?} at={at:?}: {rate:.0} Mbit/s");
        if at >= Duration::from_secs(2) {
            rates.push(rate);
        }
    }
    assert!(rates.len() >= 3, "too few steady-state samples");
    rates.sort_by(f64::total_cmp);
    quic.close(0_u32.into(), b"done");
    driving.abort();
    server.stop().await?;
    Ok(rates[rates.len() / 2])
}

/// Opens a WebTransport session on `requests`.
async fn connect(requests: &http3::client::SendRequest, path: &str) -> Result<Session, TestError> {
    let request = Request::get(format!("https://localhost{path}")).body(())?;
    Ok(Session::connect(requests, request)
        .await?
        .map_err(|refused| format!("{refused:?}"))?
        .0)
}

/// Opens a WebTransport session on its own connection.
async fn session(quic: noq::Connection, path: &str) -> Result<Session, TestError> {
    connect(&requests(quic).1, path).await
}

#[tokio::test]
async fn cancelled_download_without_reliable_reset_preserves_http3_connection() -> Result<(), TestError> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let server = quic::serve(Config::default())?;
        let quic = client(&server, false)?.connect(server.address, "localhost")?.await?;
        let (driving, requests) = requests(quic.clone());
        // A plain request keeps the connection past its sessions.
        body(&requests, "GET", "/download?bytes=1", Bytes::new()).await?;
        for _ in 0..2 {
            let session = connect(&requests, "/wt/download?bytes=4294967296").await?;
            let mut lane = session.accept_uni().await.ok_or("missing download stream")?;
            assert!(lane.read_chunk().await?.is_some());
            // The ended session stops the lane, as the server resets it, both with WT_SESSION_GONE.
            session.close(0, "").await;
            assert_eq!(lane.read_chunk().await, Err(http3::Error::Refused));
            assert!(quic.close_reason().is_none());
        }
        // The connection still carries a request once the server has reset both lanes.
        body(&requests, "GET", "/download?bytes=1", Bytes::new()).await?;
        quic.close(0_u32.into(), b"done");
        driving.abort();
        server.stop().await
    })
    .await?
}

#[tokio::test]
async fn cancelling_idle_http3_progress_releases_admission_and_preserves_the_connection() -> Result<(), TestError> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let server = quic::serve(Config::default())?;
        for reliable_reset in [false, true] {
            let endpoint = client(&server, reliable_reset)?;
            let quic = endpoint.connect(server.address, "localhost")?.await?;
            let (driving, requests) = requests(quic.clone());
            let id = upload_id(&requests).await?;
            let (response, mut recv) =
                quic::send(&requests, "GET", &format!("/upload/progress?id={id}"), Bytes::new()).await?;
            assert_eq!(response.status(), 200);
            assert!(recv.data().await?.is_some(), "missing progress ready record");
            let probe = || async {
                let probe = json(&requests, "GET", "/probe").await?;
                Ok::<_, TestError>(probe["load"]["active"].as_u64().ok_or("missing admission count")?)
            };
            assert_eq!(probe().await?, 1);
            recv.stop(Code::H3_REQUEST_CANCELLED);
            tokio::time::timeout(Duration::from_millis(500), async {
                while probe().await? != 0 {
                    tokio::task::yield_now().await;
                }
                Ok::<_, TestError>(())
            })
            .await
            .map_err(|_| "cancelled progress retained admission until its heartbeat")??;
            let download = body(&requests, "GET", "/download?bytes=1", Bytes::new()).await?;
            assert_eq!(download.len(), 1);
            assert!(quic.close_reason().is_none());
            quic.close(0_u32.into(), b"done");
            driving.abort();
        }
        server.stop().await
    })
    .await?
}

async fn assert_closed_without_error(quic: &noq::Connection) {
    match quic.closed().await {
        noq::ConnectionError::ApplicationClosed(close) => assert_eq!(close.error_code, Code::H3_NO_ERROR.into()),
        error => panic!("connection must close with H3_NO_ERROR: {error:?}"),
    }
}

/// Lets `duration` pass on the paused clock, as the server's session timers see it.
async fn jump(duration: Duration) {
    tokio::time::pause();
    tokio::time::sleep(duration).await;
    tokio::time::resume();
}

#[tokio::test]
async fn webtransport_only_connection_ends_with_its_session() -> Result<(), TestError> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let server = quic::serve(Config::default())?;
        let endpoint = client(&server, true)?;

        let mut config = server.client.clone();
        let mut transport = noq::TransportConfig::default();
        transport.stream_receive_window(16_u32.into());
        config.transport_config(Arc::new(transport));
        let refused = endpoint.connect_with(config, server.address, "localhost")?.await?;
        let upload = session(refused.clone(), "/wt/upload?id=").await?;
        let mut progress = upload.accept_uni().await.ok_or("missing progress stream")?;
        jump(Duration::from_secs(4)).await;
        let mut record = Vec::new();
        while !record.ends_with(b"\n") {
            record.extend_from_slice(&progress.read_chunk().await?.ok_or("truncated record")?);
        }
        assert!(matches!(
            graphite_meter_core::wire::decode_upload_progress(record.trim_ascii_end())?,
            graphite_meter_core::wire::UploadProgress::Error { .. }
        ));
        let credit = refused.stats().frame_rx.max_data;
        assert_eq!(credit, 0, "refused upload was granted credit");
        jump(Duration::from_secs(2)).await;
        assert_eq!(upload.closed().await?, (0, String::new()));
        upload.close(0, "").await;
        // The server that ended the session lingers so its CLOSE arrives first.
        jump(Duration::from_secs(1)).await;
        assert_closed_without_error(&refused).await;

        let ended = endpoint.connect(server.address, "localhost")?.await?;
        session(ended.clone(), "/wt/ping").await?.close(0, "").await;
        assert_closed_without_error(&ended).await;

        let stopped = endpoint.connect(server.address, "localhost")?.await?;
        let ping = session(stopped.clone(), "/wt/ping").await?;
        let QuicServer { stop, task, .. } = server;
        stop.send(()).ok();
        assert_eq!(ping.closed().await?, (4, "shutdown".into()));
        ping.close(0, "").await;
        jump(Duration::from_secs(1)).await;
        assert_closed_without_error(&stopped).await;
        task.await??;
        Ok::<_, TestError>(())
    })
    .await?
}

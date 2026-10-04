#[path = "support/http2.rs"]
mod http2;

#[path = "support/native.rs"]
mod native;

#[path = "../../test_tls.rs"]
mod test_tls;

use bytes::Bytes;
use graphite_meter_server::config::{Config, NativeKind};
use h2::{RecvStream, client::SendRequest};
use http::{Response, Version};
use rustls::pki_types::ServerName;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    task::JoinHandle,
};
use tokio_rustls::TlsConnector;

struct Harness {
    address: SocketAddr,
    connector: TlsConnector,
    client: SendRequest<Bytes>,
    driver: JoinHandle<Result<(), h2::Error>>,
    server: native::NativeServer,
}

impl Harness {
    async fn start(duration: Duration) -> Self {
        Self::start_config(Config { max_operation_duration: duration, ..Config::default() }).await
    }

    async fn start_config(config: Config) -> Self {
        let (tls, client_tls) = test_tls::configs("localhost", &[&rustls::version::TLS13], &[b"h2"]).unwrap();
        let server = native::serve(native::server(config), NativeKind::H2, Some(Arc::new(tls))).await;
        let address = server.address;
        let connector = TlsConnector::from(Arc::new(client_tls));
        let stream = connect(&connector, TcpStream::connect(address).await.unwrap()).await;
        let (client, connection) = h2::client::Builder::new()
            .initial_window_size(1024)
            .initial_connection_window_size(1024 * 1024)
            .handshake(stream)
            .await
            .unwrap();
        let driver = tokio::spawn(connection);
        Self { address, connector, client, driver, server }
    }

    async fn connect_from(&self, source: [u8; 4]) -> SendRequest<Bytes> {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind((source, 0).into()).unwrap();
        let stream = connect(&self.connector, socket.connect(self.address).await.unwrap()).await;
        let (client, connection) = h2::client::handshake(stream).await.unwrap();
        tokio::spawn(connection);
        client
    }

    /// Waits for the server to close the client's connection without error.
    async fn closed(&mut self, failure: &str) {
        let closed = tokio::time::timeout(Duration::from_secs(2), &mut self.driver).await;
        closed.expect(failure).unwrap().unwrap();
    }

    async fn close(self) {
        self.server.shutdown().await;
        self.driver.abort();
        let _ = self.driver.await;
    }
}

async fn connect(connector: &TlsConnector, socket: TcpStream) -> tokio_rustls::client::TlsStream<TcpStream> {
    let name = ServerName::try_from("localhost").unwrap();
    let stream = connector.connect(name, socket).await.unwrap();
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
    stream
}

async fn response(client: &mut SendRequest<Bytes>, method: &str, path: &str) -> Response<RecvStream> {
    std::future::poll_fn(|cx| client.poll_ready(cx)).await.unwrap();
    let (response, _) = client.send_request(http2::request(method, path), true).unwrap();
    response.await.unwrap()
}

fn parse(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body).unwrap()
}

async fn json(client: &mut SendRequest<Bytes>, method: &str, path: &str) -> serde_json::Value {
    parse(&http2::body(response(client, method, path).await.into_body()).await)
}

#[tokio::test]
async fn validated_h2_serves_probe_and_mounts_only_native_routes() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut harness = Harness::start(Duration::from_secs(3)).await;
        let probe = response(&mut harness.client, "GET", "/probe").await;
        assert_eq!(probe.status(), 200);
        assert_eq!(probe.version(), Version::HTTP_2);
        assert_eq!(probe.headers()["access-control-allow-origin"], "*");
        assert_eq!(probe.headers()["timing-allow-origin"], "*");
        assert_eq!(probe.headers()["access-control-allow-headers"], "*");
        assert!(!probe.headers().contains_key("access-control-allow-credentials"));
        assert_eq!(probe.headers()["cache-control"], "no-store");
        assert!(!probe.headers().contains_key("alt-svc"));
        assert!(!probe.headers().contains_key("connection"));
        assert_eq!(parse(&http2::body(probe.into_body()).await)["protocolNegotiated"], "h2");
        for path in ["/preflight", "/servers", "/ws/session", "/ws/ping"] {
            let reply = response(&mut harness.client, "GET", path).await;
            assert_eq!(reply.status(), 404, "{path}");
        }
        harness.close().await;
    })
    .await
    .expect("HTTP/2 lifecycle stalled");
}

#[tokio::test]
async fn expired_flow_controlled_stream_does_not_cancel_healthy_sibling() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut harness = Harness::start(Duration::from_millis(400)).await;
        let session = json(&mut harness.client, "POST", "/upload/session").await;
        let id = session["uploadId"].as_str().unwrap();
        let stalled = response(&mut harness.client, "GET", "/download?bytes=68719476736").await;
        assert_eq!(stalled.status(), 200);
        let mut stalled = stalled.into_body();
        // Withhold stream window updates while the connection stays writable.
        advance_clock(Duration::from_millis(200)).await;
        std::future::poll_fn(|cx| harness.client.poll_ready(cx)).await.unwrap();
        let (healthy_reply, mut healthy_upload) = harness
            .client
            .send_request(http2::request("POST", &format!("/upload?id={id}")), false)
            .unwrap();
        healthy_upload.send_data(Bytes::from_static(b"abc"), false).unwrap();
        let checkpoint = json(&mut harness.client, "POST", &format!("/upload/checkpoint?id={id}")).await;
        assert_eq!(checkpoint["bytes"], 3);
        advance_clock(Duration::from_millis(250)).await;
        loop {
            match stalled.data().await {
                Some(Ok(_)) => {} // Intentionally do not release this stream's capacity.
                Some(Err(error)) => {
                    assert_eq!(error.reason(), Some(h2::Reason::CANCEL));
                    break;
                }
                None => panic!("stalled response completed instead of resetting"),
            }
        }
        healthy_upload.send_data(Bytes::from_static(b"def"), true).unwrap();
        let reply = healthy_reply
            .await
            .expect("healthy upload was cancelled with its sibling");
        assert_eq!(reply.status(), 200);
        assert_eq!(parse(&http2::body(reply.into_body()).await)["bytes"], 6);
        assert_eq!(json(&mut harness.client, "GET", "/probe").await["load"]["active"], 0);
        harness.close().await;
    })
    .await
    .expect("HTTP/2 stream-local cancellation stalled");
}

#[tokio::test]
async fn shutdown_drops_active_h2_stream_futures() {
    tokio::time::timeout(Duration::from_secs(7), async {
        let mut harness = Harness::start(Duration::from_secs(30)).await;
        let reply = response(&mut harness.client, "GET", "/download?bytes=68719476736").await;
        let _stalled = reply.into_body();
        harness.close().await;
    })
    .await
    .expect("HTTP/2 stream escaped listener shutdown");
}

// Pause only while advancing: keep real network waits on the running clock so
// Tokio cannot auto-advance unrelated deadlines while socket readiness arrives.
async fn advance_clock(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
}

#[tokio::test]
async fn fifteen_seconds_idle_closes_h2_and_releases_connection_capacity() {
    let mut harness = Harness::start_config(Config {
        max_connections: 1,
        max_connections_per_client: 1,
        ..Config::default()
    })
    .await;
    // Complete one exchange so the server has processed the client's preface.
    let probe = response(&mut harness.client, "GET", "/probe").await;
    http2::body(probe.into_body()).await;
    let mut rejected = TcpStream::connect(harness.address).await.unwrap();
    assert_eq!(rejected.read(&mut [0; 1]).await.unwrap(), 0);
    advance_clock(Duration::from_secs(14)).await;
    assert!(!harness.driver.is_finished(), "closed before 15 seconds idle");
    advance_clock(Duration::from_secs(2)).await;
    harness.closed("idle connection did not close").await;
    let mut replacement = TcpStream::connect(harness.address).await.unwrap();
    let read = tokio::time::timeout(Duration::from_millis(50), replacement.read(&mut [0; 1])).await;
    assert!(read.is_err(), "idle connection retained the sole connection permit");
    drop(replacement);
    harness.server.shutdown().await;
}

#[tokio::test]
async fn active_progress_is_not_idle_and_gets_a_fresh_idle_period_when_finished() {
    let mut harness = Harness::start(Duration::from_secs(180)).await;
    let session = json(&mut harness.client, "POST", "/upload/session").await;
    let path = format!("/upload/progress?id={}", session["uploadId"].as_str().unwrap());
    let mut progress = response(&mut harness.client, "GET", &path).await.into_body();
    let ready = progress.data().await.unwrap().unwrap();
    progress.flow_control().release_capacity(ready.len()).unwrap();
    assert_eq!(parse(&ready)["type"], "ready");
    advance_clock(Duration::from_secs(61)).await;
    let idle = harness.driver.is_finished();
    assert!(!idle, "active stream was mistaken for an idle connection");
    assert_eq!(json(&mut harness.client, "GET", "/probe").await["load"]["active"], 1);
    let finished = response(&mut harness.client, "DELETE", &path).await;
    assert_eq!(finished.status(), 204);
    http2::body(finished.into_body()).await;
    let completed = http2::body(progress).await;
    assert!(std::str::from_utf8(&completed).unwrap().contains("complete"));
    advance_clock(Duration::from_secs(14)).await;
    let idle = harness.driver.is_finished();
    assert!(!idle, "active lifetime was charged to idle timeout");
    advance_clock(Duration::from_secs(2)).await;
    harness
        .closed("connection did not become idle after progress completed")
        .await;
    harness.server.shutdown().await;
}

#[tokio::test]
async fn silent_connections_from_few_sources_leave_room_for_new_clients() {
    tokio::time::timeout(Duration::from_secs(20), async {
        // The former 36 MiB reservation admitted only seven connections here.
        let harness = Harness::start_config(Config {
            max_connections: 40,
            max_connections_per_client: 8,
            max_buffer_bytes: 256 * 1024 * 1024,
            ..Config::default()
        })
        .await;
        let mut silent = Vec::new();
        for source in 2..6 {
            for _ in 0..8 {
                silent.push(harness.connect_from([127, 0, 0, source]).await);
            }
        }
        let mut client = harness.connect_from([127, 0, 0, 6]).await;
        let probe = response(&mut client, "GET", "/probe").await;
        assert_eq!(probe.status(), 200);
        http2::body(probe.into_body()).await;
        drop(silent);
        harness.close().await;
    })
    .await
    .expect("silent connections exhausted the HTTP/2 budget");
}

type Peer = tokio_rustls::client::TlsStream<TcpStream>;

async fn frame(peer: &mut Peer, kind: u8, flags: u8, stream: u32, payload: &[u8]) {
    let mut bytes = (payload.len() as u32).to_be_bytes()[1..].to_vec();
    bytes.extend([kind, flags]);
    bytes.extend(stream.to_be_bytes());
    bytes.extend(payload);
    peer.write_all(&bytes).await.unwrap();
    peer.flush().await.unwrap();
}

async fn next_frame(peer: &mut Peer) -> std::io::Result<(u8, u8, u32, Vec<u8>)> {
    let mut head = [0; 9];
    peer.read_exact(&mut head).await?;
    let mut payload = vec![0; u32::from_be_bytes([0, head[0], head[1], head[2]]) as usize];
    peer.read_exact(&mut payload).await?;
    let stream = u32::from_be_bytes(head[5..].try_into().unwrap());
    Ok((head[3], head[4], stream, payload))
}

async fn open_stream(peer: &mut Peer, id: u32, method: u8, path: &str, body: &[u8]) {
    let mut head = vec![method, 0x87, 0x04, path.len() as u8];
    head.extend(path.as_bytes());
    head.extend(b"\x01\x09localhost");
    frame(peer, 1, if body.is_empty() { 5 } else { 4 }, id, &head).await;
    if !body.is_empty() {
        frame(peer, 0, 1, id, body).await;
    }
}

async fn read_data(peer: &mut Peer, id: u32, limit: usize) -> Vec<u8> {
    let mut data = Vec::new();
    while data.len() < limit {
        let (kind, flags, stream, payload) = next_frame(peer).await.expect("response cut");
        assert_ne!((kind, stream), (3, id), "request reset");
        if stream == id && kind == 0 {
            data.extend(payload);
        }
        if stream == id && flags & 1 == 1 {
            break;
        }
    }
    data
}

async fn exchange(peer: &mut Peer, id: u32, method: u8, path: &str, body: &[u8]) -> Vec<u8> {
    open_stream(peer, id, method, path, body).await;
    read_data(peer, id, usize::MAX).await
}

const DOWNLOAD_BYTES: usize = 1024 * 1024;

async fn held_download(peer: &mut Peer, id: u32, steps: usize) -> usize {
    open_stream(peer, id, 0x82, &format!("/download?bytes={DOWNLOAD_BYTES}"), b"").await;
    let received = read_data(peer, id, 1).await.len();
    for _ in 0..steps {
        advance_clock(Duration::from_secs(6)).await;
    }
    for stream in [0, id] {
        frame(peer, 8, 0, stream, &(DOWNLOAD_BYTES as u32).to_be_bytes()).await;
    }
    received + read_data(peer, id, usize::MAX).await.len()
}

#[tokio::test]
async fn admitted_work_keeps_leftover_credit_and_unacknowledged_shutdown_does_not() {
    let harness = Harness::start(Duration::from_secs(30)).await;
    let socket = TcpStream::connect(harness.address).await.unwrap();
    let mut peer = connect(&harness.connector, socket).await;
    peer.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n").await.unwrap();
    frame(&mut peer, 4, 0, 0, &[]).await;
    let session = parse(&exchange(&mut peer, 1, 0x83, "/upload/session", b"").await);
    let path = format!("/upload?id={}", session["uploadId"].as_str().unwrap());
    assert_eq!(parse(&exchange(&mut peer, 3, 0x83, &path, b"abc").await)["bytes"], 3);
    advance_clock(Duration::from_secs(10)).await;
    let download = held_download(&mut peer, 5, 2).await;
    assert_eq!(download, DOWNLOAD_BYTES, "leftover credit cut an admitted download");
    for id in [7, 9] {
        advance_clock(Duration::from_secs(7)).await;
        exchange(&mut peer, id, 0x82, "/probe", b"").await;
    }
    advance_clock(Duration::from_secs(2)).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !matches!(next_frame(&mut peer).await.unwrap(), (6, 0, ..)) {}
    })
    .await
    .expect("the server never sent its shutdown PING");
    let download = held_download(&mut peer, 11, 1).await;
    assert_eq!(download, DOWNLOAD_BYTES, "the shutdown grace cut a download racing its GOAWAY");
    advance_clock(Duration::from_secs(5)).await;
    tokio::time::timeout(Duration::from_secs(2), async { while next_frame(&mut peer).await.is_ok() {} })
        .await
        .expect("an unacknowledged shutdown kept the upload's credit alive");
    harness.close().await;
}

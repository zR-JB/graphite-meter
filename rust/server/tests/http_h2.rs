mod support;

use bytes::Bytes;
use graphite_meter_server::{config::Config, http_server::HttpServer};
use h2::{RecvStream, client::SendRequest};
use http::{Request, Response, Version};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
};
use tokio_rustls::TlsConnector;

struct Harness {
    address: SocketAddr,
    client: SendRequest<Bytes>,
    driver: JoinHandle<Result<(), h2::Error>>,
    stop: oneshot::Sender<()>,
    server: JoinHandle<Result<(), graphite_meter_server::config::ConfigError>>,
}

impl Harness {
    async fn start(duration: Duration) -> Self {
        Self::start_config(Config {
            max_operation_duration: duration,
            ..Config::default()
        })
        .await
    }

    async fn start_config(config: Config) -> Self {
        let identity = support::Identity::generate().unwrap();
        let certificate =
            CertificateDer::from_pem_file(identity.directory().join("identity.pem")).unwrap();
        let key = PrivateKeyDer::from_pem_file(identity.directory().join("identity.key")).unwrap();
        let provider = Arc::new(graphite_meter_server::crypto::provider());
        let mut tls = ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![certificate.clone()], key)
            .unwrap();
        tls.alpn_protocols = vec![b"h2".to_vec()];
        let mut roots = RootCertStore::empty();
        roots.add(certificate).unwrap();
        let mut client_tls = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        client_tls.alpn_protocols = vec![b"h2".to_vec()];
        let server = Arc::new(HttpServer::new(Arc::new(config)).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let server = tokio::spawn(server.serve_http2(listener, Arc::new(tls), async {
            let _ = stopped.await;
        }));
        let stream = TlsConnector::from(Arc::new(client_tls))
            .connect(
                ServerName::try_from("localhost").unwrap(),
                TcpStream::connect(address).await.unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stream.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
        let (client, connection) = h2::client::Builder::new()
            .initial_window_size(1024)
            .initial_connection_window_size(1024 * 1024)
            .handshake(stream)
            .await
            .unwrap();
        let driver = tokio::spawn(connection);
        Self {
            address,
            client,
            driver,
            stop,
            server,
        }
    }

    async fn close(self) {
        self.stop.send(()).unwrap();
        self.server.await.unwrap().unwrap();
        self.driver.abort();
        let _ = self.driver.await;
    }
}

fn request(method: &str, path: &str) -> Request<()> {
    Request::builder()
        .method(method)
        .uri(format!("https://localhost{path}"))
        .version(Version::HTTP_2)
        .body(())
        .unwrap()
}

async fn response(
    client: &mut SendRequest<Bytes>,
    method: &str,
    path: &str,
    mut body: Bytes,
) -> Response<RecvStream> {
    std::future::poll_fn(|cx| client.poll_ready(cx))
        .await
        .unwrap();
    let (response, mut upload) = client
        .send_request(request(method, path), body.is_empty())
        .unwrap();
    while !body.is_empty() {
        upload.reserve_capacity(body.len().min(16 * 1024));
        let available = std::future::poll_fn(|cx| upload.poll_capacity(cx))
            .await
            .unwrap()
            .unwrap();
        let length = body.len().min(available).min(16 * 1024);
        let data = body.split_to(length);
        upload.send_data(data, body.is_empty()).unwrap();
    }
    response.await.unwrap()
}

async fn collect(mut body: RecvStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.unwrap();
        body.flow_control().release_capacity(chunk.len()).unwrap();
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

#[tokio::test]
async fn validated_h2_reuses_discovery_and_receiver_owned_upload() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut harness = Harness::start(Duration::from_secs(3)).await;
        for path in ["/probe"] {
            let response = response(&mut harness.client, "GET", path, Bytes::new()).await;
            assert_eq!(response.status(), 200);
            assert_eq!(response.version(), Version::HTTP_2);
            assert_eq!(response.headers()["access-control-allow-origin"], "*");
            let value: serde_json::Value =
                serde_json::from_slice(&collect(response.into_body()).await).unwrap();
            if path == "/probe" {
                assert_eq!(value["protocolNegotiated"], "h2");
            }
        }
        for path in ["/preflight", "/servers", "/ws/session", "/ws/ping"] {
            let reply = response(&mut harness.client, "GET", path, Bytes::new()).await;
            assert_eq!(reply.status(), 404, "{path}");
        }
        let reply = response(&mut harness.client, "POST", "/wt/session", Bytes::new()).await;
        assert_eq!(reply.status(), 200);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&collect(reply.into_body()).await).unwrap(),
            serde_json::json!({"token":"","expires":0})
        );
        let reply = response(
            &mut harness.client,
            "GET",
            "/download?bytes=300000",
            Bytes::new(),
        )
        .await;
        let downloaded = collect(reply.into_body()).await;
        assert_eq!(downloaded.len(), 300000);
        assert_eq!(&downloaded[..37856], &downloaded[262144..]);
        let reply = response(&mut harness.client, "POST", "/upload/session", Bytes::new()).await;
        let session: serde_json::Value =
            serde_json::from_slice(&collect(reply.into_body()).await).unwrap();
        let id = session["uploadId"].as_str().unwrap();
        let reply = response(
            &mut harness.client,
            "POST",
            &format!("/upload?id={id}"),
            downloaded.into(),
        )
        .await;
        assert_eq!(reply.status(), 200);
        let upload: serde_json::Value =
            serde_json::from_slice(&collect(reply.into_body()).await).unwrap();
        assert_eq!(upload["bytes"], 300000);
        let reply = response(
            &mut harness.client,
            "POST",
            &format!("/upload/checkpoint?id={id}"),
            Bytes::new(),
        )
        .await;
        let checkpoint: serde_json::Value =
            serde_json::from_slice(&collect(reply.into_body()).await).unwrap();
        assert_eq!(checkpoint["bytes"], 300000);
        harness.close().await;
    })
    .await
    .expect("HTTP/2 lifecycle stalled");
}

#[tokio::test]
async fn expired_flow_controlled_stream_does_not_cancel_healthy_sibling() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut harness = Harness::start(Duration::from_millis(400)).await;
        let reply = response(&mut harness.client, "POST", "/upload/session", Bytes::new()).await;
        let session: serde_json::Value =
            serde_json::from_slice(&collect(reply.into_body()).await).unwrap();
        let id = session["uploadId"].as_str().unwrap();
        let stalled = response(
            &mut harness.client,
            "GET",
            "/download?bytes=68719476736",
            Bytes::new(),
        )
        .await;
        assert_eq!(stalled.status(), 200);
        let mut stalled = stalled.into_body();
        // Withhold stream window updates while the connection stays writable.
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::future::poll_fn(|cx| harness.client.poll_ready(cx))
            .await
            .unwrap();
        let (healthy_reply, mut healthy_upload) = harness
            .client
            .send_request(request("POST", &format!("/upload?id={id}")), false)
            .unwrap();
        healthy_upload
            .send_data(Bytes::from_static(b"abc"), false)
            .unwrap();
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
        healthy_upload
            .send_data(Bytes::from_static(b"def"), true)
            .unwrap();
        let reply = healthy_reply
            .await
            .expect("healthy upload was cancelled with its sibling");
        assert_eq!(reply.status(), 200);
        let value: serde_json::Value =
            serde_json::from_slice(&collect(reply.into_body()).await).unwrap();
        assert_eq!(value["bytes"], 6);
        let probe = response(&mut harness.client, "GET", "/probe", Bytes::new()).await;
        let probe: serde_json::Value =
            serde_json::from_slice(&collect(probe.into_body()).await).unwrap();
        assert_eq!(probe["load"]["active"], 0);
        harness.close().await;
    })
    .await
    .expect("HTTP/2 stream-local cancellation stalled");
}

#[tokio::test]
async fn shutdown_drops_active_h2_stream_futures() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut harness = Harness::start(Duration::from_secs(30)).await;
        let reply = response(
            &mut harness.client,
            "GET",
            "/download?bytes=68719476736",
            Bytes::new(),
        )
        .await;
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
async fn sixty_seconds_idle_closes_h2_and_releases_connection_capacity() {
    let mut harness = Harness::start_config(Config {
        max_connections: 1,
        max_connections_per_client: 1,
        ..Config::default()
    })
    .await;
    // Complete one exchange so the server has processed the client's preface.
    let probe = response(&mut harness.client, "GET", "/probe", Bytes::new()).await;
    collect(probe.into_body()).await;
    let mut rejected = TcpStream::connect(harness.address).await.unwrap();
    assert_eq!(rejected.read(&mut [0; 1]).await.unwrap(), 0);
    advance_clock(Duration::from_secs(59)).await;
    assert!(
        !harness.driver.is_finished(),
        "closed before 60 seconds idle"
    );
    advance_clock(Duration::from_secs(2)).await;
    tokio::time::timeout(Duration::from_secs(2), &mut harness.driver)
        .await
        .expect("idle connection did not close")
        .unwrap()
        .unwrap();
    let mut replacement = TcpStream::connect(harness.address).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), replacement.read(&mut [0; 1]))
            .await
            .is_err(),
        "idle connection retained the sole connection permit"
    );
    drop(replacement);
    harness.stop.send(()).unwrap();
    harness.server.await.unwrap().unwrap();
}

#[tokio::test]
async fn active_progress_is_not_idle_and_gets_a_fresh_idle_period_when_finished() {
    let mut harness = Harness::start(Duration::from_secs(180)).await;
    let reply = response(&mut harness.client, "POST", "/upload/session", Bytes::new()).await;
    let session: serde_json::Value =
        serde_json::from_slice(&collect(reply.into_body()).await).unwrap();
    let id = session["uploadId"].as_str().unwrap();
    let path = format!("/upload/progress?id={id}");
    let reply = response(&mut harness.client, "GET", &path, Bytes::new()).await;
    let mut progress = reply.into_body();
    let ready = progress.data().await.unwrap().unwrap();
    progress
        .flow_control()
        .release_capacity(ready.len())
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&ready).unwrap()["type"],
        "ready"
    );
    advance_clock(Duration::from_secs(61)).await;
    assert!(
        !harness.driver.is_finished(),
        "active stream was mistaken for an idle connection"
    );
    let probe = response(&mut harness.client, "GET", "/probe", Bytes::new()).await;
    let probe: serde_json::Value =
        serde_json::from_slice(&collect(probe.into_body()).await).unwrap();
    assert_eq!(probe["load"]["active"], 1);
    let finished = response(&mut harness.client, "DELETE", &path, Bytes::new()).await;
    assert_eq!(finished.status(), 204);
    collect(finished.into_body()).await;
    let completed = collect(progress).await;
    assert!(
        std::str::from_utf8(&completed)
            .unwrap()
            .contains("complete")
    );
    advance_clock(Duration::from_secs(59)).await;
    assert!(
        !harness.driver.is_finished(),
        "active lifetime was charged to idle timeout"
    );
    advance_clock(Duration::from_secs(2)).await;
    tokio::time::timeout(Duration::from_secs(2), &mut harness.driver)
        .await
        .expect("connection did not become idle after progress completed")
        .unwrap()
        .unwrap();
    harness.stop.send(()).unwrap();
    harness.server.await.unwrap().unwrap();
}

mod support;

use graphite_meter_server::{config::Config, http_server::HttpServer};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig, SupportedProtocolVersion,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_rustls::{TlsConnector, client::TlsStream};

fn configs(identity: &support::Identity) -> (Arc<ServerConfig>, RootCertStore) {
    let certificate =
        CertificateDer::from_pem_file(identity.directory().join("identity.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_file(identity.directory().join("identity.key")).unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(certificate.clone()).unwrap();
    let mut server =
        ServerConfig::builder_with_provider(Arc::new(graphite_meter_server::crypto::provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .unwrap();
    server.alpn_protocols = vec![b"http/1.1".to_vec()];
    (Arc::new(server), roots)
}

fn connector(roots: RootCertStore, version: &'static SupportedProtocolVersion) -> TlsConnector {
    let mut client =
        ClientConfig::builder_with_provider(Arc::new(graphite_meter_server::crypto::provider()))
            .with_protocol_versions(&[version])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    client.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsConnector::from(Arc::new(client))
}

async fn connect(address: SocketAddr, connector: &TlsConnector) -> TlsStream<TcpStream> {
    let stream = connector
        .connect(
            ServerName::try_from("localhost").unwrap(),
            TcpStream::connect(address).await.unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        stream.get_ref().1.protocol_version(),
        Some(rustls::ProtocolVersion::TLSv1_3)
    );
    assert_eq!(
        stream.get_ref().1.alpn_protocol(),
        Some(b"http/1.1".as_slice())
    );
    stream
}

async fn request(
    address: SocketAddr,
    connector: &TlsConnector,
    method: &str,
    path: &str,
    body: &[u8],
) -> (String, Vec<u8>) {
    let mut socket = connect(address, connector).await;
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
    socket.write_all(body).await.unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.unwrap();
    let boundary = response
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap();
    (
        String::from_utf8(response[..boundary].to_vec()).unwrap(),
        response[boundary + 4..].to_vec(),
    )
}

#[tokio::test]
async fn validated_tls13_serves_discovery_download_and_upload_after_rejected_tls12() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let identity = support::Identity::generate().unwrap();
        let (tls, roots) = configs(&identity);
        let good = connector(roots.clone(), &rustls::version::TLS13);
        let old = connector(roots, &rustls::version::TLS12);
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let serving = tokio::spawn(server.serve_https1(listener, tls, async {
            let _ = stopped.await;
        }));
        assert!(
            old.connect(
                ServerName::try_from("localhost").unwrap(),
                TcpStream::connect(address).await.unwrap()
            )
            .await
            .is_err()
        );
        for path in ["/probe", "/preflight", "/servers"] {
            let (headers, body) = request(address, &good, "GET", path, b"").await;
            assert!(headers.starts_with("HTTP/1.1 200"));
            assert!(headers.contains("access-control-allow-origin: *"));
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if path == "/probe" {
                assert_eq!(value["protocolNegotiated"], "http/1.1");
            }
        }
        for path in ["/wt/session", "/ws/session"] {
            let (headers, body) = request(address, &good, "POST", path, b"").await;
            assert!(headers.starts_with("HTTP/1.1 200"));
            assert!(headers.contains("cache-control: no-store"));
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                serde_json::json!({"token":"","expires":0})
            );
            let (headers, _) = request(address, &good, "GET", path, b"").await;
            assert!(headers.starts_with("HTTP/1.1 405"));
        }
        let (headers, download) =
            request(address, &good, "GET", "/download?bytes=300000", b"").await;
        assert!(headers.starts_with("HTTP/1.1 200"));
        assert_eq!(download.len(), 300000);
        assert_eq!(&download[..37856], &download[262144..]);
        let (_, session) = request(address, &good, "POST", "/upload/session", b"").await;
        let session: serde_json::Value = serde_json::from_slice(&session).unwrap();
        let id = session["uploadId"].as_str().unwrap();
        let (headers, upload) = request(
            address,
            &good,
            "POST",
            &format!("/upload?id={id}"),
            &download,
        )
        .await;
        assert!(headers.starts_with("HTTP/1.1 200"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&upload).unwrap()["bytes"],
            300000
        );
        let (_, checkpoint) = request(
            address,
            &good,
            "POST",
            &format!("/upload/checkpoint?id={id}"),
            b"",
        )
        .await;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&checkpoint).unwrap()["bytes"],
            300000
        );
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    })
    .await
    .expect("HTTPS lifecycle stalled");
}

#[tokio::test]
async fn tls_stalled_download_keeps_deadline_through_encrypted_writes() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let identity = support::Identity::generate().unwrap();
        let (tls, roots) = configs(&identity);
        let connector = connector(roots, &rustls::version::TLS13);
        let config = Config {
            max_operation_duration: Duration::from_millis(300),
            ..Config::default()
        };
        let server = Arc::new(HttpServer::new(Arc::new(config)).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let serving = tokio::spawn(server.serve_https1(listener, tls, async {
            let _ = stopped.await;
        }));
        let mut stalled = connect(address, &connector).await;
        stalled
            .write_all(b"GET /download?bytes=68719476736 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stalled.read_u8().await.unwrap());
        }
        assert!(headers.starts_with(b"HTTP/1.1 200"));
        let (_, probe) = request(address, &connector, "GET", "/probe", b"").await;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&probe).unwrap()["load"]["active"],
            1
        );
        loop {
            let (_, probe) = request(address, &connector, "GET", "/probe", b"").await;
            if serde_json::from_slice::<serde_json::Value>(&probe).unwrap()["load"]["active"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(stalled);
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    })
    .await
    .expect("stalled TLS write kept its admission permit");
}

#[tokio::test]
async fn shutdown_joins_incomplete_tls_handshake_and_releases_connection() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let identity = support::Identity::generate().unwrap();
        let (tls, _) = configs(&identity);
        let config = Config {
            max_connections: 1,
            max_connections_per_client: 1,
            ..Config::default()
        };
        let server = Arc::new(HttpServer::new(Arc::new(config)).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let serving = tokio::spawn(server.serve_https1(listener, tls, async {
            let _ = stopped.await;
        }));
        let mut pending = TcpStream::connect(address).await.unwrap();
        pending.write_all(b"\x16\x03\x01").await.unwrap();
        // Exhaustion proves the first socket acquired its permit before TLS.
        let mut rejected = TcpStream::connect(address).await.unwrap();
        assert_eq!(rejected.read(&mut [0; 1]).await.unwrap(), 0);
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), serving)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let result = pending.read(&mut [0; 1]).await;
        assert!(matches!(result, Ok(0) | Err(_)));
    })
    .await
    .expect("handshake task escaped listener shutdown");
}

#[tokio::test]
async fn h3_tcp_bootstrap_only_serves_probe_and_advertises_the_effective_public_port() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let identity = support::Identity::generate().unwrap();
        let (tls, roots) = configs(&identity);
        let connector = connector(roots, &rustls::version::TLS13);
        for (origin, port) in [
            ("", None),
            ("https://localhost:9443", Some(9443)),
            ("https://localhost", Some(443)),
        ] {
            let mut config = Config::default();
            config.native[graphite_meter_server::config::NativeKind::H3 as usize].public_origin =
                origin.into();
            let server = Arc::new(HttpServer::new(Arc::new(config)).unwrap());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = oneshot::channel();
            let serving =
                tokio::spawn(server.serve_https_bootstrap(listener, tls.clone(), async {
                    let _ = stopped.await;
                }));
            let (headers, body) = request(address, &connector, "GET", "/probe", b"").await;
            assert!(headers.starts_with("HTTP/1.1 200"));
            assert!(headers.contains(&format!(
                "alt-svc: h3=\":{}\"",
                port.unwrap_or(address.port())
            )));
            assert!(headers.contains("connection: close"));
            let probe: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(probe["protocolNegotiated"], "http/1.1");
            for path in [
                "/",
                "/login",
                "/preflight",
                "/servers",
                "/download",
                "/upload",
                "/ws/ping",
            ] {
                let (headers, _) = request(address, &connector, "GET", path, b"").await;
                assert!(headers.starts_with("HTTP/1.1 404"), "{path}: {headers}");
                assert!(!headers.contains("alt-svc:"));
            }
            let (headers, _) = request(address, &connector, "OPTIONS", "/probe", b"").await;
            assert!(headers.starts_with("HTTP/1.1 204"));
            stop.send(()).unwrap();
            serving.await.unwrap().unwrap();
        }
    })
    .await
    .unwrap();
}

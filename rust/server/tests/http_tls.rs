#[path = "support/http1.rs"]
mod http1;

#[path = "support/native.rs"]
mod native;

mod support;

use graphite_meter_server::config::{Config, NativeKind};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig, SupportedProtocolVersion,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{TlsConnector, client::TlsStream};

/// A TLS 1.3 server configuration for a fresh identity, and roots that trust it.
fn configs() -> (Arc<ServerConfig>, RootCertStore) {
    let identity = support::Identity::generate();
    let certificate = CertificateDer::from_pem_file(identity.directory().join("identity.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_file(identity.directory().join("identity.key")).unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(certificate.clone()).unwrap();
    let server = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .unwrap();
    (Arc::new(server), roots)
}

fn connector(roots: RootCertStore, version: &'static SupportedProtocolVersion) -> TlsConnector {
    let mut client = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[version])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsConnector::from(Arc::new(client))
}

async fn listen(config: Config, kind: NativeKind, tls: Arc<ServerConfig>) -> native::NativeServer {
    native::serve(native::server(config), kind, Some(tls)).await
}

async fn handshake(address: SocketAddr, connector: &TlsConnector) -> std::io::Result<TlsStream<TcpStream>> {
    let socket = TcpStream::connect(address).await.unwrap();
    let name = ServerName::try_from("localhost").unwrap();
    connector.connect(name, socket).await
}

async fn connect(address: SocketAddr, connector: &TlsConnector) -> TlsStream<TcpStream> {
    let stream = handshake(address, connector).await.unwrap();
    let connection = stream.get_ref().1;
    assert_eq!(connection.protocol_version(), Some(rustls::ProtocolVersion::TLSv1_3));
    assert_eq!(connection.alpn_protocol(), Some(b"http/1.1".as_slice()));
    stream
}

async fn request(
    address: SocketAddr,
    connector: &TlsConnector,
    method: &str,
    path: &str,
    body: &[u8],
) -> (String, Vec<u8>) {
    http1::exchange(connect(address, connector).await, method, path, "localhost", "", body).await
}

#[tokio::test]
async fn validated_tls13_serves_probe_after_rejected_tls12() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (tls, roots) = configs();
        let good = connector(roots.clone(), &rustls::version::TLS13);
        let old = connector(roots, &rustls::version::TLS12);
        let listener = listen(Config::default(), NativeKind::H1Tls, tls).await;
        assert!(handshake(listener.address, &old).await.is_err());
        let (headers, body) = request(listener.address, &good, "GET", "/probe", b"").await;
        assert!(headers.starts_with("HTTP/1.1 200"));
        assert!(headers.contains("access-control-allow-origin: *"));
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["protocolNegotiated"], "http/1.1");
        listener.shutdown().await;
    })
    .await
    .expect("HTTPS lifecycle stalled");
}

#[tokio::test]
async fn tls_stalled_download_keeps_deadline_through_encrypted_writes() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (tls, roots) = configs();
        let connector = connector(roots, &rustls::version::TLS13);
        let config = Config {
            max_operation_duration: Duration::from_millis(300),
            ..Config::default()
        };
        let listener = listen(config, NativeKind::H1Tls, tls).await;
        let mut stalled = connect(listener.address, &connector).await;
        let download = b"GET /download?bytes=68719476736 HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stalled.write_all(download).await.unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stalled.read_u8().await.unwrap());
        }
        assert!(headers.starts_with(b"HTTP/1.1 200"));
        let active = async || {
            let (_, probe) = request(listener.address, &connector, "GET", "/probe", b"").await;
            serde_json::from_slice::<serde_json::Value>(&probe).unwrap()["load"]["active"].clone()
        };
        assert_eq!(active().await, 1);
        while active().await != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(stalled);
        listener.shutdown().await;
    })
    .await
    .expect("stalled TLS write kept its admission permit");
}

#[tokio::test]
async fn shutdown_joins_incomplete_tls_handshake_and_releases_connection() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (tls, _) = configs();
        let config = Config {
            max_connections: 1,
            max_connections_per_client: 1,
            ..Config::default()
        };
        let mut listener = listen(config, NativeKind::H1Tls, tls).await;
        let mut pending = TcpStream::connect(listener.address).await.unwrap();
        pending.write_all(b"\x16\x03\x01").await.unwrap();
        // Exhaustion proves the first socket acquired its permit before TLS.
        let mut rejected = TcpStream::connect(listener.address).await.unwrap();
        assert_eq!(rejected.read(&mut [0; 1]).await.unwrap(), 0);
        listener.stop();
        tokio::time::timeout(Duration::from_secs(1), listener.shutdown())
            .await
            .unwrap();
        let result = pending.read(&mut [0; 1]).await;
        assert!(matches!(result, Ok(0) | Err(_)));
    })
    .await
    .expect("handshake task escaped listener shutdown");
}

#[tokio::test]
async fn h3_tcp_companion_serves_probe_and_control_routes_and_advertises_the_effective_public_port() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (tls, roots) = configs();
        let connector = connector(roots, &rustls::version::TLS13);
        for (origin, port) in [("", None), ("https://localhost:9443", Some(9443)), ("https://localhost", Some(443))] {
            let mut config = Config::default();
            config.native[NativeKind::H3 as usize].public_origin = origin.into();
            let listener = listen(config, NativeKind::H3, tls.clone()).await;
            let (headers, body) = request(listener.address, &connector, "GET", "/probe", b"").await;
            assert!(headers.starts_with("HTTP/1.1 200"));
            assert!(headers.contains(&format!("alt-svc: h3=\":{}\"", port.unwrap_or(listener.address.port()))));
            assert!(headers.contains("connection: close"));
            let probe: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(probe["protocolNegotiated"], "http/1.1");
            let (headers, body) = request(listener.address, &connector, "POST", "/upload/session", b"").await;
            let fresh = headers.starts_with("HTTP/1.1 200") && !headers.contains("alt-svc:");
            assert!(fresh, "{headers}");
            let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let id = session["uploadId"].as_str().unwrap();
            let not_found = ["/", "/login", "/preflight", "/servers", "/download", "/upload", "/ws/ping"];
            for (method, path, status) in [
                ("POST", format!("/upload/checkpoint?id={id}"), "400"),
                ("DELETE", format!("/upload/progress?id={id}"), "400"),
                ("POST", "/wt/session".into(), "200"),
                ("GET", "/upload/session".into(), "405"),
                ("OPTIONS", "/probe".into(), "204"),
            ]
            .into_iter()
            .chain(not_found.map(|path| ("GET", path.into(), "404")))
            {
                let (headers, _) = request(listener.address, &connector, method, &path, b"").await;
                let expected = format!("HTTP/1.1 {status}");
                assert!(headers.starts_with(&expected), "{method} {path}: {headers}");
                assert!(!headers.contains("alt-svc:"));
            }
            listener.shutdown().await;
        }
    })
    .await
    .unwrap();
}

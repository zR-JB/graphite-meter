//! Real QUIC coverage for the shared HTTP/3 measurement adapter.
mod support;

use bytes::{Buf, Bytes};
use graphite_meter_server::{config::Config, http_server::HttpServer};
use http::Request;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{error::Error, sync::Arc, time::Duration};
use tokio::sync::oneshot;

type TestError = Box<dyn Error + Send + Sync>;

#[tokio::test]
async fn h3_shared_upload_routes_and_stalled_stream_deadline_preserve_siblings() {
    tokio::time::timeout(Duration::from_secs(10), exercise())
        .await
        .expect("HTTP/3 adapter timed out")
        .unwrap();
}

async fn exercise() -> Result<(), TestError> {
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(4096_u32.into());
    let (mut sender, driver, stop, task) = serve_quic(
        Config {
            max_operation_duration: Duration::from_millis(250),
            ..Config::default()
        },
        transport,
    )
    .await?;
    let request = |method: &str, path: &str| {
        Request::builder()
            .method(method)
            .uri(format!("https://localhost{path}"))
            .body(())
            .unwrap()
    };
    for path in ["/preflight", "/servers", "/ws/session", "/ws/ping"] {
        let mut stream = sender.send_request(request("GET", path)).await?;
        stream.finish().await?;
        assert_eq!(stream.recv_response().await?.status(), 404, "{path}");
    }
    let mut probe = sender.send_request(request("GET", "/probe")).await?;
    probe.finish().await?;
    let response = probe.recv_response().await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("alt-svc"));
    assert!(!response.headers().contains_key("connection"));
    let mut body = Vec::new();
    while let Some(mut data) = probe.recv_data().await? {
        body.extend_from_slice(&data.copy_to_bytes(data.remaining()));
    }
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body)?["protocolNegotiated"],
        "h3"
    );
    let mut mint = sender.send_request(request("POST", "/upload/session")).await?;
    mint.finish().await?;
    assert_eq!(mint.recv_response().await?.status(), 200);
    let mut bytes = Vec::new();
    while let Some(mut data) = mint.recv_data().await? {
        bytes.extend_from_slice(&data.copy_to_bytes(data.remaining()));
    }
    let id = serde_json::from_slice::<serde_json::Value>(&bytes)?["uploadId"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut upload = sender
        .send_request(request("POST", &format!("/upload?id={id}")))
        .await?;
    upload.send_data(Bytes::from_static(b"QUIC upload")).await?;
    upload.finish().await?;
    assert_eq!(upload.recv_response().await?.status(), 200);
    let mut bytes = Vec::new();
    while let Some(mut data) = upload.recv_data().await? {
        bytes.extend_from_slice(&data.copy_to_bytes(data.remaining()));
    }
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes)?["bytes"], 11);
    let mut stalled = sender.send_request(request("GET", "/download?bytes=10000000")).await?;
    stalled.finish().await?;
    assert_eq!(stalled.recv_response().await?.status(), 200);
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(350)).await;
    tokio::time::resume();
    loop {
        match stalled.recv_data().await {
            Ok(Some(_)) => continue,
            Err(_) => break,
            Ok(None) => panic!("flow-controlled transfer must be reset at its deadline"),
        }
    }
    let mut sibling = sender.send_request(request("GET", "/download?bytes=13")).await?;
    sibling.finish().await?;
    assert_eq!(sibling.recv_response().await?.status(), 200);
    let mut count = 0;
    while let Some(data) = sibling.recv_data().await? {
        count += data.remaining();
    }
    assert_eq!(count, 13);
    stop.send(()).unwrap();
    task.await??;
    driver.abort();
    Ok(())
}
type Served = (
    h3::client::SendRequest<h3_noq::OpenStreams, Bytes>,
    tokio::task::JoinHandle<h3::error::ConnectionError>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), graphite_meter_server::config::ConfigError>>,
);

async fn serve_quic(config: Config, client: quinn::TransportConfig) -> Result<Served, TestError> {
    let identity = support::Identity::generate();
    let certificate = CertificateDer::from_pem_file(identity.directory().join("identity.pem"))?;
    let key = PrivateKeyDer::from_pem_file(identity.directory().join("identity.key"))?;
    let provider = Arc::new(graphite_meter_server::crypto::provider());
    let mut tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key)?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let server = Arc::new(HttpServer::new(Arc::new(config))?);
    let endpoint = server.quic_endpoint(Arc::new(tls), "127.0.0.1:0".parse()?)?;
    let address = endpoint.local_addr()?;
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.serve_quic(endpoint, async {
        let _ = stopped.await;
    }));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate)?;
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut config = quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(tls)?));
    config.transport_config(Arc::new(client));
    let client = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    let connection = client.connect_with(config, address, "localhost")?.await?;
    let (mut driver, sender) = h3::client::new(h3_noq::Connection::new(connection)).await?;
    let driving = tokio::spawn(async move { driver.wait_idle().await });
    Ok((sender, driving, stop, task))
}

async fn body(
    sender: &mut h3::client::SendRequest<h3_noq::OpenStreams, Bytes>,
    method: http::Method,
    path: &str,
    upload: &'static [u8],
) -> Result<Vec<u8>, TestError> {
    let request = Request::builder()
        .method(method)
        .uri(format!("https://localhost{path}"))
        .body(())?;
    let mut request = sender.send_request(request).await?;
    if !upload.is_empty() {
        request.send_data(Bytes::from_static(upload)).await?;
    }
    request.finish().await?;
    assert_eq!(request.recv_response().await?.status(), http::StatusCode::OK);
    let mut bytes = Vec::new();
    while let Some(mut data) = request.recv_data().await? {
        bytes.extend_from_slice(&data.copy_to_bytes(data.remaining()));
    }
    Ok(bytes)
}

#[tokio::test]
async fn idle_http3_connection_does_not_consume_the_shutdown_drain() -> Result<(), TestError> {
    let (mut sender, driving, stop, task) = serve_quic(Config::default(), quinn::TransportConfig::default()).await?;
    body(&mut sender, http::Method::GET, "/download?bytes=1", b"").await?;
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task).await???;
    driving.abort();
    Ok(())
}

#[tokio::test]
async fn probes_do_not_keep_admitted_works_leftover_credit_alive() -> Result<(), TestError> {
    let (mut sender, driving, stop, task) = serve_quic(Config::default(), quinn::TransportConfig::default()).await?;
    let session = body(&mut sender, http::Method::POST, "/upload/session", b"").await?;
    let session: serde_json::Value = serde_json::from_slice(&session)?;
    let path = format!("https://localhost/upload?id={}", session["uploadId"].as_str().unwrap());
    let mut upload = sender.send_request(Request::post(path).body(())?).await?;
    upload.send_data(Bytes::from_static(b"abc")).await?;
    // A round trip later the server is still admitted, awaiting the body's end.
    body(&mut sender, http::Method::GET, "/probe", b"").await?;
    upload.finish().await?;
    assert_eq!(upload.recv_response().await?.status(), http::StatusCode::OK);
    while upload.recv_data().await?.is_some() {}
    for _ in 0..2 {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(7)).await;
        tokio::time::resume();
        body(&mut sender, http::Method::GET, "/probe", b"").await?;
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(2), driving)
        .await
        .expect("probes kept the post-upload connection alive")?;
    drop(stop);
    task.abort();
    Ok(())
}

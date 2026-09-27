//! Real QUIC coverage for the shared HTTP/3 measurement adapter.
mod support;

use bytes::Bytes;
use graphite_meter_http3::{RecvHalf, client};
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
    let (requests, driver, stop, task) = serve_quic(
        Config {
            max_operation_duration: Duration::from_millis(250),
            ..Config::default()
        },
        transport,
    )
    .await?;
    for path in ["/preflight", "/servers", "/ws/session", "/ws/ping"] {
        let (response, _) = send(&requests, "GET", path, b"").await?;
        assert_eq!(response.status(), 404, "{path}");
    }
    let (response, mut probe) = send(&requests, "GET", "/probe", b"").await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("alt-svc"));
    assert!(!response.headers().contains_key("connection"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&read(&mut probe).await?)?["protocolNegotiated"],
        "h3"
    );
    let session = body(&requests, "POST", "/upload/session", b"").await?;
    let id = serde_json::from_slice::<serde_json::Value>(&session)?["uploadId"]
        .as_str()
        .unwrap()
        .to_owned();
    let upload = body(&requests, "POST", &format!("/upload?id={id}"), b"QUIC upload").await?;
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&upload)?["bytes"], 11);
    let (response, mut stalled) = send(&requests, "GET", "/download?bytes=10000000", b"").await?;
    assert_eq!(response.status(), 200);
    advance(Duration::from_millis(350)).await;
    loop {
        match stalled.data().await {
            Ok(Some(_)) => continue,
            Err(_) => break,
            Ok(None) => panic!("flow-controlled transfer must be reset at its deadline"),
        }
    }
    assert_eq!(body(&requests, "GET", "/download?bytes=13", b"").await?.len(), 13);
    stop.send(()).unwrap();
    task.await??;
    driver.abort();
    Ok(())
}

type Served = (
    client::SendRequest,
    tokio::task::JoinHandle<Result<(), graphite_meter_http3::Error>>,
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
    let (mut driver, requests) = client::new(connection);
    let driving = tokio::spawn(async move { driver.drive().await });
    Ok((requests, driving, stop, task))
}

async fn send(
    requests: &client::SendRequest,
    method: &str,
    path: &str,
    upload: &'static [u8],
) -> Result<(http::Response<()>, RecvHalf), TestError> {
    let request = Request::builder()
        .method(method)
        .uri(format!("https://localhost{path}"))
        .body(())?;
    let (mut send, mut recv) = requests.send_request(request).await?.split();
    if !upload.is_empty() {
        send.send_data(Bytes::from_static(upload)).await?;
    }
    send.finish().await?;
    Ok((recv.response().await?, recv))
}

async fn read(recv: &mut RecvHalf) -> Result<Vec<u8>, TestError> {
    let mut bytes = Vec::new();
    while let Some(data) = recv.data().await? {
        bytes.extend_from_slice(&data);
    }
    Ok(bytes)
}

async fn body(
    requests: &client::SendRequest,
    method: &str,
    path: &str,
    upload: &'static [u8],
) -> Result<Vec<u8>, TestError> {
    let (response, mut recv) = send(requests, method, path, upload).await?;
    assert_eq!(response.status(), http::StatusCode::OK);
    read(&mut recv).await
}

#[tokio::test]
async fn idle_http3_connection_does_not_consume_the_shutdown_drain() -> Result<(), TestError> {
    let (requests, driving, stop, task) = serve_quic(Config::default(), quinn::TransportConfig::default()).await?;
    body(&requests, "GET", "/download?bytes=1", b"").await?;
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task).await???;
    driving.abort();
    Ok(())
}

async fn advance(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
}

#[tokio::test]
async fn admitted_work_keeps_leftover_credit_and_probes_do_not() -> Result<(), TestError> {
    let (requests, driving, stop, task) = serve_quic(Config::default(), quinn::TransportConfig::default()).await?;
    let session = body(&requests, "POST", "/upload/session", b"").await?;
    let session: serde_json::Value = serde_json::from_slice(&session)?;
    let path = format!("https://localhost/upload?id={}", session["uploadId"].as_str().unwrap());
    let (mut upload, mut reply) = requests.send_request(Request::post(path).body(())?).await?.split();
    upload.send_data(Bytes::from_static(b"abc")).await?;
    // A round trip later the server is still admitted, awaiting the body's end.
    body(&requests, "GET", "/probe", b"").await?;
    upload.finish().await?;
    assert_eq!(reply.response().await?.status(), http::StatusCode::OK);
    read(&mut reply).await?;
    advance(Duration::from_secs(10)).await;
    let bytes = 8 * 1024 * 1024;
    let (response, mut download) = send(&requests, "GET", &format!("/download?bytes={bytes}"), b"").await?;
    assert_eq!(response.status(), http::StatusCode::OK);
    for _ in 0..2 {
        advance(Duration::from_secs(6)).await;
    }
    assert_eq!(
        read(&mut download).await?.len(),
        bytes,
        "leftover credit cut an admitted download"
    );
    for _ in 0..2 {
        advance(Duration::from_secs(7)).await;
        body(&requests, "GET", "/probe", b"").await?;
    }
    advance(Duration::from_secs(2)).await;
    tokio::time::timeout(Duration::from_secs(2), driving)
        .await
        .expect("probes kept the post-upload connection alive")??;
    drop(stop);
    task.abort();
    Ok(())
}

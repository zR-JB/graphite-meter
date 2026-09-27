//! Real QUIC coverage for the shared HTTP/3 measurement adapter.
mod support;

use bytes::{Buf, Bytes};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_server::{config::Config, http_server::HttpServer};
use http::Request;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    error::Error,
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};
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
    let identity = support::Identity::generate();
    let certificate = CertificateDer::from_pem_file(identity.directory().join("identity.pem"))?;
    let key = PrivateKeyDer::from_pem_file(identity.directory().join("identity.key"))?;
    let provider = Arc::new(graphite_meter_server::crypto::provider());
    let mut tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key)?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut config =
        quinn::ServerConfig::with_crypto(Arc::new(quinn::crypto::rustls::QuicServerConfig::try_from(tls)?));
    let mut transport = quinn::TransportConfig::default();
    transport.send_window(64 * 1024);
    transport.max_concurrent_bidi_streams(256_u32.into());
    config.transport_config(Arc::new(transport));
    let server_endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse()?)?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate)?;
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut client_config = quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(tls)?));
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(4096_u32.into());
    client_config.transport_config(Arc::new(transport));
    let client_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    client_endpoint.set_default_client_config(client_config);
    let connecting = client_endpoint.connect(server_endpoint.local_addr()?, "localhost")?;
    let (client_quic, (server_quic, peer)) =
        tokio::try_join!(async { Ok::<_, TestError>(connecting.await?) }, async {
            let incoming = server_endpoint.accept().await.ok_or("closed")?;
            let peer = incoming.remote_address();
            Ok::<_, TestError>((incoming.await?, peer))
        })?;
    let server = Arc::new(HttpServer::new(Arc::new(Config {
        max_operation_duration: Duration::from_millis(250),
        ..Config::default()
    }))?);
    let credit = server.receive_credit(server_quic.clone());
    let mut connection = h3::server::builder()
        .max_field_section_size(32 * 1024)
        .build(h3_noq::Connection::new(server_quic))
        .await?;
    let (stop, stopped) = oneshot::channel();
    let serving = tokio::spawn(async move {
        let mut requests = FuturesUnordered::new();
        let active_responses = Arc::new(AtomicUsize::new(0));
        tokio::pin!(stopped);
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                Some(_) = requests.next() => {},
                accepted = connection.accept() => {
                    let Some(resolver) = accepted? else {break;};
                    assert!(requests.len() < 256);
                    let server = server.clone();
                    let (credit, active_responses) = (credit.clone(), active_responses.clone());
                    requests.push(async move {
                        let (request, stream) = resolver.resolve_request().await?;
                        // Cancellation is local to this owned request future.
                        let _ = server.serve_http3_request(request, stream, peer, credit, active_responses).await;
                        Ok::<_,TestError>(())
                    });
                }
            }
        }
        Ok::<_, TestError>(())
    });
    let (mut connection, mut sender) = h3::client::new(h3_noq::Connection::new(client_quic)).await?;
    let driver = tokio::spawn(async move { connection.wait_idle().await });
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
    let _ = stop.send(());
    serving.await??;
    driver.abort();
    let _ = driver.await;
    client_endpoint.close(0_u32.into(), b"done");
    server_endpoint.close(0_u32.into(), b"done");
    Ok(())
}
type Served = (
    h3::client::SendRequest<h3_noq::OpenStreams, Bytes>,
    tokio::task::JoinHandle<h3::error::ConnectionError>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), graphite_meter_server::config::ConfigError>>,
);

async fn serve_quic() -> Result<Served, TestError> {
    let identity = support::Identity::generate();
    let certificate = CertificateDer::from_pem_file(identity.directory().join("identity.pem"))?;
    let key = PrivateKeyDer::from_pem_file(identity.directory().join("identity.key"))?;
    let provider = Arc::new(graphite_meter_server::crypto::provider());
    let mut tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key)?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let server = Arc::new(HttpServer::new(Arc::new(Config::default()))?);
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
    let client = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    client.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)?,
    )));
    let connection = client.connect(address, "localhost")?.await?;
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
    let (mut sender, driving, stop, task) = serve_quic().await?;
    body(&mut sender, http::Method::GET, "/download?bytes=1", b"").await?;
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task).await???;
    driving.abort();
    Ok(())
}

#[tokio::test]
async fn probes_do_not_keep_admitted_works_leftover_credit_alive() -> Result<(), TestError> {
    let (mut sender, driving, stop, task) = serve_quic().await?;
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

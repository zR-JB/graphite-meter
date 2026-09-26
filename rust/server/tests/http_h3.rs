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
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls)?,
    ));
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
    let mut client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)?,
    ));
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
                    let active_responses = active_responses.clone();
                    requests.push(async move {
                        let (request, stream) = resolver.resolve_request().await?;
                        // Cancellation is local to this owned request future.
                        let _ = server.serve_http3_request(request, stream, peer, active_responses).await;
                        Ok::<_,TestError>(())
                    });
                }
            }
        }
        Ok::<_, TestError>(())
    });
    let (mut connection, mut sender) =
        h3::client::new(h3_noq::Connection::new(client_quic)).await?;
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
    let mut mint = sender
        .send_request(request("POST", "/upload/session"))
        .await?;
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
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes)?["bytes"],
        11
    );
    let mut stalled = sender
        .send_request(request("GET", "/download?bytes=10000000"))
        .await?;
    stalled.finish().await?;
    assert_eq!(stalled.recv_response().await?.status(), 200);
    tokio::time::sleep(Duration::from_millis(350)).await;
    loop {
        match stalled.recv_data().await {
            Ok(Some(_)) => continue,
            Err(_) => break,
            Ok(None) => panic!("flow-controlled transfer must be reset at its deadline"),
        }
    }
    let mut sibling = sender
        .send_request(request("GET", "/download?bytes=13"))
        .await?;
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

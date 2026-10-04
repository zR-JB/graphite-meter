//! Real QUIC coverage for the shared HTTP/3 measurement adapter.
#[path = "support/quic.rs"]
mod quic;

use bytes::Bytes;
use graphite_meter_http3::client::SendRequest;
use graphite_meter_server::config::Config;
use http::Request;
use quic::{QuicServer, TestError, body, json, read, send};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn h3_routes_and_stalled_stream_deadline_preserve_siblings() {
    tokio::time::timeout(Duration::from_secs(10), exercise())
        .await
        .expect("HTTP/3 adapter timed out")
        .unwrap();
}

async fn exercise() -> Result<(), TestError> {
    let mut transport = noq::TransportConfig::default();
    transport.stream_receive_window(4096_u32.into());
    let config = Config {
        max_operation_duration: Duration::from_millis(250),
        ..Config::default()
    };
    let (server, driver, requests) = serve_quic(config, transport).await?;
    for path in ["/preflight", "/servers", "/ws/session", "/ws/ping"] {
        let (response, _) = send(&requests, "GET", path, Bytes::new()).await?;
        assert_eq!(response.status(), 404, "{path}");
    }
    let (response, mut probe) = send(&requests, "GET", "/probe", Bytes::new()).await?;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("alt-svc"));
    assert!(!response.headers().contains_key("connection"));
    let negotiated = serde_json::from_slice::<serde_json::Value>(&read(&mut probe).await?)?;
    assert_eq!(negotiated["protocolNegotiated"], "h3");
    let (response, mut stalled) = send(&requests, "GET", "/download?bytes=10000000", Bytes::new()).await?;
    assert_eq!(response.status(), 200);
    advance(Duration::from_millis(350)).await;
    loop {
        match stalled.data().await {
            Ok(Some(_)) => continue,
            Err(_) => break,
            Ok(None) => panic!("flow-controlled transfer must be reset at its deadline"),
        }
    }
    assert_eq!(
        body(&requests, "GET", "/download?bytes=13", Bytes::new()).await?.len(),
        13
    );
    server.stop().await?;
    driver.abort();
    Ok(())
}

/// A server, and the driver and request sender of a client connection to it with `transport`.
async fn serve_quic(
    config: Config,
    transport: noq::TransportConfig,
) -> Result<
    (
        QuicServer,
        tokio::task::JoinHandle<Result<(), graphite_meter_http3::Error>>,
        SendRequest,
    ),
    TestError,
> {
    let server = quic::serve(config)?;
    let mut config = server.client.clone();
    config.transport_config(Arc::new(transport));
    let client = noq::Endpoint::client("127.0.0.1:0".parse()?)?;
    let connection = client.connect_with(config, server.address, "localhost")?.await?;
    let (driving, requests) = quic::requests(connection);
    Ok((server, driving, requests))
}

#[tokio::test]
async fn idle_http3_connection_does_not_consume_the_shutdown_drain() -> Result<(), TestError> {
    let (server, driving, requests) = serve_quic(Config::default(), noq::TransportConfig::default()).await?;
    body(&requests, "GET", "/download?bytes=1", Bytes::new()).await?;
    tokio::time::timeout(Duration::from_secs(2), server.stop()).await??;
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
    let (server, driving, requests) = serve_quic(Config::default(), noq::TransportConfig::default()).await?;
    let session = json(&requests, "POST", "/upload/session").await?;
    let path = format!("https://localhost/upload?id={}", session["uploadId"].as_str().unwrap());
    let (mut upload, mut reply) = requests.send_request(Request::post(path).body(())?).await?.split();
    upload.send_data(Bytes::from_static(b"abc")).await?;
    // A round trip later the server is still admitted, awaiting the body's end.
    body(&requests, "GET", "/probe", Bytes::new()).await?;
    upload.finish().await?;
    assert_eq!(reply.response().await?.status(), http::StatusCode::OK);
    read(&mut reply).await?;
    advance(Duration::from_secs(10)).await;
    let bytes = 8 * 1024 * 1024;
    let (response, mut download) = send(&requests, "GET", &format!("/download?bytes={bytes}"), Bytes::new()).await?;
    assert_eq!(response.status(), http::StatusCode::OK);
    for _ in 0..2 {
        advance(Duration::from_secs(6)).await;
    }
    let received = read(&mut download).await?.len();
    assert_eq!(received, bytes, "leftover credit cut an admitted download");
    for _ in 0..2 {
        advance(Duration::from_secs(7)).await;
        body(&requests, "GET", "/probe", Bytes::new()).await?;
    }
    advance(Duration::from_secs(2)).await;
    tokio::time::timeout(Duration::from_secs(2), driving)
        .await
        .expect("probes kept the post-upload connection alive")??;
    let QuicServer { stop, task, .. } = server;
    drop(stop);
    task.abort();
    Ok(())
}

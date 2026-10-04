#[path = "support/http1.rs"]
mod http1;

#[path = "support/native.rs"]
mod native;

use graphite_meter_server::config::{Config, NativeKind};
use graphite_meter_server::http::HttpServer;
use serde_json::Value;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

async fn listen(config: Config) -> native::NativeServer {
    let server = Arc::new(HttpServer::new(config.validated().unwrap()).unwrap());
    native::serve(server, NativeKind::H1, None).await
}

#[tokio::test]
async fn real_http1_serves_discovery_and_streams_exact_download_then_joins_shutdown() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = listen(Config {
            server_name: "Local meter".into(),
            server_location: "Berlin".into(),
            ..Config::default()
        })
        .await;

        for (path, origin) in [
            ("/servers", "http://meter.example:7246"),
            ("http://[2001:db8::1]/servers", "http://[2001:db8::1]:7246"),
        ] {
            let socket = TcpStream::connect(listener.address).await.unwrap();
            let (headers, body) = http1::exchange(socket, "GET", path, "meter.example:80", "", b"").await;
            assert!(headers.starts_with("HTTP/1.1 200"));
            let value: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["servers"][0]["name"], "Local meter");
            assert_eq!(value["servers"][0]["location"], "Berlin");
            assert_eq!(value["servers"][0]["additionalOrigins"], serde_json::json!([origin]));
        }
        let (headers, body) = request(listener.address, "GET", "/download?bytes=300000", "", b"").await;
        assert!(headers.starts_with("HTTP/1.1 200"));
        assert_eq!(body.len(), 300000);
        assert_eq!(&body[..37856], &body[262144..]);
        assert!(body.iter().any(|byte| *byte != 0));
        listener.shutdown().await;
    })
    .await
    .expect("HTTP listener or shutdown stalled");
}

#[tokio::test]
async fn stalled_download_releases_capacity_at_request_deadline() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut config = Config {
            max_operation_duration: Duration::from_millis(500),
            ..Config::default()
        };
        config.limits.operations_per_client = 1;
        config.limits.sessions_per_client = 1;
        let listener = listen(config).await;
        let mut stalled = TcpStream::connect(listener.address).await.unwrap();
        stalled
            .write_all(b"GET /download?bytes=68719476736 HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        // Read only the headers; leave the large response blocked in TCP.
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stalled.read_u8().await.unwrap());
        }
        assert!(headers.starts_with(b"HTTP/1.1 200"));
        // The stalled download holds this client's only permit.
        let (download, _) = request(listener.address, "GET", "/download?bytes=1", "", b"").await;
        assert!(download.starts_with("HTTP/1.1 429"), "{download}");
        advance_http1_clock(Duration::from_millis(550)).await;
        until_idle(listener.address).await;
        let (download, _) = request(listener.address, "GET", "/download?bytes=1", "", b"").await;
        assert!(download.starts_with("HTTP/1.1 200"), "{download}");
        // Keep the non-reading peer alive until after recovery is observed.
        drop(stalled);
        listener.shutdown().await;
    })
    .await
    .expect("stalled download retained its admission slot");
}

#[tokio::test]
async fn oversized_http1_headers_are_rejected() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = listen(Config::default()).await;
        let mut socket = TcpStream::connect(listener.address).await.unwrap();
        let request = format!(
            "GET /probe HTTP/1.1\r\nHost: localhost\r\nX-Large: {}\r\nConnection: close\r\n\r\n",
            "a".repeat(40 * 1024)
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        // A reset after the error response is allowed because unread input remains.
        let _ = socket.read_to_end(&mut response).await;
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 431"), "{response}");
        listener.shutdown().await;
    })
    .await
    .expect("oversized header request stalled");
}

#[tokio::test]
async fn http1_upload_refusals_preserve_owner_and_unread_body_boundaries() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = listen(Config {
            trusted_proxies: vec!["127.0.0.0/8".parse().unwrap()],
            ..Config::default()
        })
        .await;
        let address = listener.address;
        let id = upload_id(address, "192.0.2.1").await;
        let path = format!("/upload?id={id}");
        let (rejected, body) = request(address, "PUT", &path, "192.0.2.1", b"unread").await;
        assert!(rejected.starts_with("HTTP/1.1 400") && rejected.contains("connection: close"));
        assert_eq!(body, b"request body not accepted\n");
        // Minting and refusing an unread body do not establish upload ownership.
        let (headers, _) = request(address, "POST", &path, "192.0.2.1", b"").await;
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        let checkpoint = format!("/upload/checkpoint?id={id}");
        let (headers, _) = request(address, "POST", &checkpoint, "192.0.2.1", b"").await;
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        let (headers, _) = request(address, "POST", &checkpoint, "192.0.2.2", b"").await;
        assert!(headers.starts_with("HTTP/1.1 403"));
        assert!(headers.contains("x-graphite-upload-refusal: ownerMismatch"));
        let (headers, _) = request(address, "POST", "/upload?id=invalid", "192.0.2.1", b"bad").await;
        assert!(headers.starts_with("HTTP/1.1 400"));
        assert!(headers.contains("x-graphite-upload-refusal: invalid"));
        listener.shutdown().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn ambiguous_proxy_evidence_owns_no_upload() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = listen(Config {
            trusted_proxies: vec!["127.0.0.0/8".parse().unwrap()],
            ..Config::default()
        })
        .await;
        let address = listener.address;
        // The proxy forwards a client on its own address, such as a local health check.
        let id = upload_id(address, "127.0.0.1").await;
        let (headers, _) = request(address, "POST", &format!("/upload?id={id}"), "127.0.0.1", b"proxied").await;
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        // Without X-Real-IP the proxy names no client: as in Go, that owns nothing, not the proxy's own address.
        let checkpoint = format!("/upload/checkpoint?id={id}");
        let (headers, body) = request(address, "POST", &checkpoint, "", b"").await;
        assert!(headers.starts_with("HTTP/1.1 403"), "{headers}");
        let refusal = "x-graphite-upload-refusal: ownerMismatch";
        assert!(headers.contains(refusal), "{headers}");
        assert_eq!(body, b"upload id belongs to another client\n");
        let (headers, _) = request(address, "POST", &checkpoint, "127.0.0.1", b"").await;
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        listener.shutdown().await;
    })
    .await
    .expect("upload ownership check stalled");
}

#[tokio::test]
async fn stalled_upload_read_releases_capacity_and_keeps_received_bytes() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = listen(Config {
            max_operation_duration: Duration::from_millis(200),
            ..Config::default()
        })
        .await;
        let id = upload_id(listener.address, "").await;
        let mut stalled = TcpStream::connect(listener.address).await.unwrap();
        let head = format!("POST /upload?id={id} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100000\r\n\r\nabc");
        stalled.write_all(head.as_bytes()).await.unwrap();
        let checkpoint = format!("/upload/checkpoint?id={id}");
        while json(listener.address, "POST", &checkpoint).await["bytes"] != 3 {
            tokio::task::yield_now().await;
        }
        advance_http1_clock(Duration::from_millis(250)).await;
        until_idle(listener.address).await;
        assert_eq!(json(listener.address, "POST", &checkpoint).await["bytes"], 3);
        drop(stalled);
        listener.shutdown().await;
    })
    .await
    .expect("upload read deadline failed to release capacity");
}

/// An exchange on its own connection, from the client a trusted proxy names in X-Real-IP unless `owner` is empty.
async fn request(address: SocketAddr, method: &str, path: &str, owner: &str, body: &[u8]) -> (String, Vec<u8>) {
    let mut socket = TcpStream::connect(address).await.unwrap();
    let owner = if owner.is_empty() { String::new() } else { format!("X-Real-IP: {owner}\r\n") };
    http1::exchange(&mut socket, method, path, "localhost", &owner, body).await
}

/// A reply's JSON body, or null for any other body.
async fn json(address: SocketAddr, method: &str, path: &str) -> Value {
    let (_, body) = request(address, method, path, "", b"").await;
    serde_json::from_slice(&body).unwrap_or_default()
}

async fn upload_id(address: SocketAddr, owner: &str) -> String {
    let (_, session) = request(address, "POST", "/upload/session", owner, b"").await;
    let session: Value = serde_json::from_slice(&session).unwrap();
    session["uploadId"].as_str().unwrap().to_owned()
}

/// Waits until the probe reports no admitted work.
async fn until_idle(address: SocketAddr) {
    while json(address, "GET", "/probe").await["load"]["active"] != 0 {
        tokio::task::yield_now().await;
    }
}

async fn read_headers(reader: &mut BufReader<TcpStream>) -> String {
    let mut headers = String::new();
    while !headers.ends_with("\r\n\r\n") {
        assert!(reader.read_line(&mut headers).await.unwrap() > 0);
    }
    headers
}

async fn progress_event(reader: &mut BufReader<TcpStream>) -> Value {
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        let count = usize::from_str_radix(line.trim(), 16).unwrap();
        assert!(count > 0, "progress stream ended before expected event");
        let mut chunk = vec![0; count + 2];
        reader.read_exact(&mut chunk).await.unwrap();
        let record = std::str::from_utf8(&chunk[..count]).unwrap().trim();
        if !record.is_empty() {
            return serde_json::from_str(record).unwrap();
        }
    }
}

async fn advance_http1_clock(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
}

/// A connection that sent `request`, read through its first reply's headers, which it returns.
async fn sent(address: SocketAddr, request: &str) -> (BufReader<TcpStream>, String) {
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut socket = BufReader::new(socket);
    let headers = read_headers(&mut socket).await;
    (socket, headers)
}

/// The read that follows a connection's close.
async fn closed(socket: &mut BufReader<TcpStream>) {
    let read = tokio::time::timeout(Duration::from_secs(1), socket.read(&mut [0; 1])).await;
    assert_eq!(read.unwrap().unwrap(), 0);
}

#[tokio::test]
async fn keepalive_idle_uses_fifteen_seconds_and_releases_connection_capacity() {
    let listener = listen(Config {
        max_connections: 1,
        max_connections_per_client: 1,
        ..Config::default()
    })
    .await;
    let (mut socket, headers) =
        sent(listener.address, "GET /download?bytes=0 HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
    assert!(headers.starts_with("HTTP/1.1 200"));
    advance_http1_clock(Duration::from_secs(14)).await;
    let read = tokio::time::timeout(Duration::from_millis(20), socket.read(&mut [0; 1])).await;
    assert!(read.is_err(), "keepalive closed before 15 seconds");
    let mut rejected = TcpStream::connect(listener.address).await.unwrap();
    assert_eq!(rejected.read(&mut [0; 1]).await.unwrap(), 0);
    advance_http1_clock(Duration::from_secs(2)).await;
    closed(&mut socket).await;
    let (reply, _) = request(listener.address, "GET", "/probe", "", b"").await;
    assert!(reply.starts_with("HTTP/1.1 200"));
    listener.shutdown().await;
}

#[tokio::test]
async fn active_http1_progress_survives_an_idle_interval() {
    let listener = listen(Config {
        max_operation_duration: Duration::from_secs(180),
        ..Config::default()
    })
    .await;
    let id = upload_id(listener.address, "").await;
    let progress = format!("GET /upload/progress?id={id} HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let (mut progress, headers) = sent(listener.address, &progress).await;
    assert!(headers.contains("application/x-ndjson"));
    assert!(headers.contains("no-store, no-transform"));
    assert!(headers.contains("x-accel-buffering: no"));
    assert_eq!(progress_event(&mut progress).await["type"], "ready");
    advance_http1_clock(Duration::from_secs(61)).await;
    let path = format!("/upload/progress?id={id}");
    let (headers, _) = request(listener.address, "DELETE", &path, "", b"").await;
    assert!(headers.starts_with("HTTP/1.1 204"));
    assert_eq!(progress_event(&mut progress).await["type"], "complete");
    drop(progress);
    listener.shutdown().await;
}

#[tokio::test]
async fn prefetched_partial_pipeline_still_has_a_finite_idle_bound() {
    let listener = listen(Config::default()).await;
    let pipeline = "GET /download?bytes=0 HTTP/1.1\r\nHost: localhost\r\n\r\nGET /probe HTTP/1.1\r\nHost:";
    let (mut socket, headers) = sent(listener.address, pipeline).await;
    assert!(headers.starts_with("HTTP/1.1 200"));
    // Hyper can prefetch this partial next header during the first request.
    // Its unread buffer is private, so this corner may retain the 60s idle
    // bound instead of Go's 10s header bound. It must never remain unbounded.
    advance_http1_clock(Duration::from_secs(61)).await;
    closed(&mut socket).await;
    listener.shutdown().await;
}

#[tokio::test]
async fn upload_idle_returns_refusal_and_preserves_receiver_bytes() {
    let listener = listen(Config::default()).await;
    let id = upload_id(listener.address, "").await;
    let mut socket = TcpStream::connect(listener.address).await.unwrap();
    let head = format!(
        "POST /upload?id={id} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\nConnection: close\r\n\r\na"
    );
    socket.write_all(head.as_bytes()).await.unwrap();
    let checkpoint = format!("/upload/checkpoint?id={id}");
    tokio::time::timeout(Duration::from_secs(1), async {
        while json(listener.address, "POST", &checkpoint).await["bytes"] != 1 {}
    })
    .await
    .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::resume();
    let mut response = Vec::new();
    let _ = socket.read_to_end(&mut response).await;
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 408"), "{response}");
    assert!(response.contains("x-graphite-upload-refusal: idle"));
    assert_eq!(json(listener.address, "POST", &checkpoint).await["bytes"], 1);
    listener.shutdown().await;
}

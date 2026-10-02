#[path = "support/http1.rs"]
mod http1;

#[path = "support/native.rs"]
mod native;

use graphite_meter_server::config::{Config, NativeKind};
use graphite_meter_server::http::HttpServer;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

#[tokio::test]
async fn real_http1_serves_discovery_and_streams_exact_download_then_joins_shutdown() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let config = Config {
            server_name: "Local meter".into(),
            server_location: "Berlin".into(),
            ..Config::default()
        };
        let server = Arc::new(HttpServer::new(config.validated().unwrap()).unwrap());
        let listener = native::serve(server, NativeKind::H1, None).await;

        let mut generation = None;
        for (method, path, status) in [
            ("GET", "/preflight", 200),
            ("POST", "/preflight", 405),
            ("HEAD", "/preflight", 200),
            ("GET", "http://other.example/preflight", 200),
            ("GET", "/servers", 200),
            ("GET", "http://[2001:db8::1]/servers", 200),
            ("POST", "/servers", 405),
            ("HEAD", "/servers", 200),
            ("OPTIONS", "/servers", 204),
            ("GET", "/probe", 200),
            ("DELETE", "/probe", 405),
            ("HEAD", "/probe", 200),
            ("GET", "/unknown", 404),
            ("POST", "/login", 404),
            ("GET", "/auth/session", 404),
            ("GET", "/download?bytes=300000", 200),
        ] {
            let socket = TcpStream::connect(listener.address).await.unwrap();
            let (headers, body) = http1::exchange(socket, method, path, "meter.example:80", "", b"").await;
            assert!(
                headers.starts_with(&format!("HTTP/1.1 {status}")),
                "{method} {path}: {headers}"
            );
            if status == 405 {
                // As Go's "/" pattern, the app answers a method no route on this listener allows.
                assert!(headers.contains("allow: GET, HEAD\r\n"), "{headers}");
            } else if status != 200 || method == "HEAD" {
                if status == 204 || method == "HEAD" {
                    assert!(body.is_empty());
                }
            } else if path.starts_with("/download") {
                assert_eq!(body.len(), 300000);
                assert_eq!(&body[..37856], &body[262144..]);
                assert!(body.iter().any(|byte| *byte != 0));
            } else {
                assert!(headers.contains("content-type: application/json"));
                assert!(headers.contains("cache-control: no-store"));
                let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                if path.ends_with("/servers") {
                    let origin = if path.starts_with("http:") {
                        "http://[2001:db8::1]:7246"
                    } else {
                        "http://meter.example:7246"
                    };
                    assert_eq!(value["defaultSelection"], serde_json::json!(["self"]));
                    assert_eq!(value["servers"][0]["name"], "Local meter");
                    assert_eq!(value["servers"][0]["location"], "Berlin");
                    assert_eq!(value["servers"][0]["additionalOrigins"], serde_json::json!([origin]));
                } else if path.ends_with("/preflight") {
                    if let Some(first) = &generation {
                        assert_eq!(&value["generation"], first);
                    } else {
                        generation = Some(value["generation"].clone());
                    }
                    if path.starts_with("http:") {
                        assert_eq!(
                            value["capabilities"]["throughput"][0]["baseUrl"],
                            "http://other.example:7246"
                        );
                    }
                }
            }
        }
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
        let server = Arc::new(HttpServer::new(config.validated().unwrap()).unwrap());
        let listener = native::serve(server.clone(), NativeKind::H1, None).await;
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
        let download = fetch(listener.address, "/download?bytes=1").await;
        assert!(
            download.starts_with(b"HTTP/1.1 429"),
            "{}",
            String::from_utf8_lossy(&download)
        );
        advance_http1_clock(Duration::from_millis(550)).await;
        loop {
            let probe = fetch(listener.address, "/probe").await;
            let boundary = probe.windows(4).position(|part| part == b"\r\n\r\n").unwrap();
            let document: serde_json::Value = serde_json::from_slice(&probe[boundary + 4..]).unwrap();
            if document["load"]["active"] == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        let download = fetch(listener.address, "/download?bytes=1").await;
        assert!(
            download.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&download)
        );
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
        let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
        let listener = native::serve(server, NativeKind::H1, None).await;
        let mut socket = TcpStream::connect(listener.address).await.unwrap();
        let request = format!(
            "GET /probe HTTP/1.1\r\nHost: localhost\r\nX-Large: {}\r\nConnection: close\r\n\r\n",
            "a".repeat(40 * 1024)
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        // A reset after the error response is allowed because unread input remains.
        let _ = socket.read_to_end(&mut response).await;
        assert!(
            response.starts_with(b"HTTP/1.1 431"),
            "{}",
            String::from_utf8_lossy(&response)
        );
        listener.shutdown().await;
    })
    .await
    .expect("oversized header request stalled");
}

async fn fetch(address: SocketAddr, path: &str) -> Vec<u8> {
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn real_upload_lifecycle_uses_receiver_totals_and_owner_refusals() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let config = Config {
            trusted_proxies: vec!["127.0.0.0/8".parse().unwrap()],
            ..Config::default()
        };
        let server = Arc::new(HttpServer::new(config.validated().unwrap()).unwrap());
        let listener = native::serve(server, NativeKind::H1, None).await;
        let (headers, session) = upload_request(listener.address, "POST", "/upload/session", "192.0.2.1", b"").await;
        assert!(headers.starts_with("HTTP/1.1 200"));
        assert!(headers.contains("access-control-allow-origin: *"));
        let session: serde_json::Value = serde_json::from_slice(&session).unwrap();
        let id = session["uploadId"].as_str().unwrap();
        let mut socket = TcpStream::connect(listener.address).await.unwrap();
        socket
            .write_all(
                format!("GET /upload/progress?id={id} HTTP/1.1\r\nHost: localhost\r\nX-Real-IP: 192.0.2.1\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut progress = tokio::io::BufReader::new(socket);
        let headers = read_headers(&mut progress).await;
        assert!(headers.contains("application/x-ndjson"));
        assert!(headers.contains("no-store, no-transform"));
        assert!(headers.contains("x-accel-buffering: no"));
        assert_eq!(progress_event(&mut progress).await["type"], "ready");

        let path = format!("/upload?id={id}");
        let first = vec![1; 1234];
        let second = vec![2; 5678];
        let (rejected, body) = upload_request(listener.address, "PUT", &path, "192.0.2.1", &second).await;
        assert!(rejected.starts_with("HTTP/1.1 400") && rejected.contains("connection: close"));
        assert_eq!(body, b"request body not accepted\n");
        let (first, second) = tokio::join!(
            upload_request(listener.address, "POST", &path, "192.0.2.1", &first),
            upload_request(listener.address, "POST", &path, "192.0.2.1", &second),
        );
        for (reply, expected) in [(first, 1234), (second, 5678)] {
            assert!(reply.0.starts_with("HTTP/1.1 200"));
            let value: serde_json::Value = serde_json::from_slice(&reply.1).unwrap();
            assert_eq!(value["bytes"], expected);
        }
        let checkpoint = format!("/upload/checkpoint?id={id}");
        let (headers, bytes) = upload_request(listener.address, "POST", &checkpoint, "192.0.2.1", b"").await;
        assert!(headers.starts_with("HTTP/1.1 200"));
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["bytes"], 6912);
        assert!(value["nanos"].as_u64().unwrap() > 0);
        let (headers, _) = upload_request(listener.address, "POST", &checkpoint, "192.0.2.2", b"").await;
        assert!(headers.starts_with("HTTP/1.1 403"));
        assert!(headers.contains("x-graphite-upload-refusal: ownerMismatch"));
        let (headers, _) = upload_request(listener.address, "POST", "/upload?id=invalid", "192.0.2.1", b"bad").await;
        assert!(headers.starts_with("HTTP/1.1 400"));
        assert!(headers.contains("x-graphite-upload-refusal: invalid"));
        let (headers, _) = upload_request(listener.address, "GET", "/upload/session", "192.0.2.1", b"").await;
        assert!(headers.starts_with("HTTP/1.1 404"), "{headers}");

        let (headers, body) = upload_request(
            listener.address,
            "DELETE",
            &format!("/upload/progress?id={id}"),
            "192.0.2.1",
            b"",
        )
        .await;
        assert!(headers.starts_with("HTTP/1.1 204"));
        assert!(body.is_empty());
        loop {
            let event = progress_event(&mut progress).await;
            if event["type"] == "complete" {
                assert_eq!(event["bytes"], 6912);
                break;
            }
        }
        drop(progress);
        listener.shutdown().await;
    })
    .await
    .expect("upload lifecycle stalled");
}

#[tokio::test]
async fn ambiguous_proxy_evidence_owns_no_upload() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let config = Config {
            trusted_proxies: vec!["127.0.0.0/8".parse().unwrap()],
            ..Config::default()
        };
        let server = Arc::new(HttpServer::new(config.validated().unwrap()).unwrap());
        let listener = native::serve(server, NativeKind::H1, None).await;
        // The proxy forwards a client on its own address, such as a local health check.
        let (_, session) = upload_request(listener.address, "POST", "/upload/session", "127.0.0.1", b"").await;
        let session: serde_json::Value = serde_json::from_slice(&session).unwrap();
        let id = session["uploadId"].as_str().unwrap();
        let (headers, _) = upload_request(
            listener.address,
            "POST",
            &format!("/upload?id={id}"),
            "127.0.0.1",
            b"proxied",
        )
        .await;
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        // Without X-Real-IP the proxy names no client: as in Go, that owns nothing, not the proxy's own address.
        let checkpoint = format!("/upload/checkpoint?id={id}");
        let (headers, body) = upload_request(listener.address, "POST", &checkpoint, "", b"").await;
        assert!(headers.starts_with("HTTP/1.1 403"), "{headers}");
        assert!(
            headers.contains("x-graphite-upload-refusal: ownerMismatch"),
            "{headers}"
        );
        assert_eq!(body, b"upload id belongs to another client\n");
        let (headers, _) = upload_request(listener.address, "POST", &checkpoint, "127.0.0.1", b"").await;
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        listener.shutdown().await;
    })
    .await
    .expect("upload ownership check stalled");
}

#[tokio::test]
async fn stalled_upload_read_releases_capacity_and_keeps_received_bytes() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let config = Config {
            max_operation_duration: Duration::from_millis(200),
            ..Config::default()
        };
        let server = Arc::new(HttpServer::new(config.validated().unwrap()).unwrap());
        let listener = native::serve(server, NativeKind::H1, None).await;
        let (_, session) = upload_request(listener.address, "POST", "/upload/session", "", b"").await;
        let session: serde_json::Value = serde_json::from_slice(&session).unwrap();
        let id = session["uploadId"].as_str().unwrap();
        let mut stalled = TcpStream::connect(listener.address).await.unwrap();
        stalled
            .write_all(
                format!("POST /upload?id={id} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100000\r\n\r\nabc")
                    .as_bytes(),
            )
            .await
            .unwrap();
        loop {
            let (_, checkpoint) = upload_request(
                listener.address,
                "POST",
                &format!("/upload/checkpoint?id={id}"),
                "",
                b"",
            )
            .await;
            if serde_json::from_slice::<serde_json::Value>(&checkpoint).is_ok_and(|value| value["bytes"] == 3) {
                break;
            }
            tokio::task::yield_now().await;
        }
        advance_http1_clock(Duration::from_millis(250)).await;
        loop {
            let (_, probe) = upload_request(listener.address, "GET", "/probe", "", b"").await;
            let probe: serde_json::Value = serde_json::from_slice(&probe).unwrap();
            if probe["load"]["active"] == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        let (_, checkpoint) = upload_request(
            listener.address,
            "POST",
            &format!("/upload/checkpoint?id={id}"),
            "",
            b"",
        )
        .await;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&checkpoint).unwrap()["bytes"],
            3
        );
        drop(stalled);
        listener.shutdown().await;
    })
    .await
    .expect("upload read deadline failed to release capacity");
}

async fn upload_request(address: SocketAddr, method: &str, path: &str, owner: &str, body: &[u8]) -> (String, Vec<u8>) {
    let mut socket = TcpStream::connect(address).await.unwrap();
    let owner = if owner.is_empty() {
        String::new()
    } else {
        format!("X-Real-IP: {owner}\r\n")
    };
    http1::exchange(&mut socket, method, path, "localhost", &owner, body).await
}

async fn read_headers(reader: &mut tokio::io::BufReader<TcpStream>) -> String {
    use tokio::io::AsyncBufReadExt;
    let mut headers = String::new();
    while !headers.ends_with("\r\n\r\n") {
        assert!(reader.read_line(&mut headers).await.unwrap() > 0);
    }
    headers
}

async fn progress_event(reader: &mut tokio::io::BufReader<TcpStream>) -> serde_json::Value {
    use tokio::io::AsyncBufReadExt;
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

#[tokio::test]
async fn keepalive_idle_uses_fifteen_seconds_and_releases_connection_capacity() {
    let server = Arc::new(
        HttpServer::new(
            Config {
                max_connections: 1,
                max_connections_per_client: 1,
                ..Config::default()
            }
            .validated()
            .unwrap(),
        )
        .unwrap(),
    );
    let listener = native::serve(server, NativeKind::H1, None).await;
    let mut socket = TcpStream::connect(listener.address).await.unwrap();
    socket
        .write_all(b"GET /download?bytes=0 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut socket = tokio::io::BufReader::new(socket);
    assert!(read_headers(&mut socket).await.starts_with("HTTP/1.1 200"));
    advance_http1_clock(Duration::from_secs(14)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), socket.read(&mut [0; 1]))
            .await
            .is_err(),
        "keepalive closed before 15 seconds"
    );
    let mut rejected = TcpStream::connect(listener.address).await.unwrap();
    assert_eq!(rejected.read(&mut [0; 1]).await.unwrap(), 0);
    advance_http1_clock(Duration::from_secs(2)).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), socket.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let reply = fetch(listener.address, "/probe").await;
    assert!(reply.starts_with(b"HTTP/1.1 200"));
    listener.shutdown().await;
}

#[tokio::test]
async fn active_http1_progress_survives_an_idle_interval() {
    let server = Arc::new(
        HttpServer::new(
            Config {
                max_operation_duration: Duration::from_secs(180),
                ..Config::default()
            }
            .validated()
            .unwrap(),
        )
        .unwrap(),
    );
    let listener = native::serve(server, NativeKind::H1, None).await;
    let (_, session) = upload_request(listener.address, "POST", "/upload/session", "", b"").await;
    let session: serde_json::Value = serde_json::from_slice(&session).unwrap();
    let id = session["uploadId"].as_str().unwrap();
    let mut socket = TcpStream::connect(listener.address).await.unwrap();
    socket
        .write_all(format!("GET /upload/progress?id={id} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut progress = tokio::io::BufReader::new(socket);
    read_headers(&mut progress).await;
    assert_eq!(progress_event(&mut progress).await["type"], "ready");
    advance_http1_clock(Duration::from_secs(61)).await;
    let (headers, _) = upload_request(
        listener.address,
        "DELETE",
        &format!("/upload/progress?id={id}"),
        "",
        b"",
    )
    .await;
    assert!(headers.starts_with("HTTP/1.1 204"));
    assert_eq!(progress_event(&mut progress).await["type"], "complete");
    drop(progress);
    listener.shutdown().await;
}

#[tokio::test]
async fn prefetched_partial_pipeline_still_has_a_finite_idle_bound() {
    let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
    let listener = native::serve(server, NativeKind::H1, None).await;
    let mut socket = TcpStream::connect(listener.address).await.unwrap();
    socket
        .write_all(b"GET /download?bytes=0 HTTP/1.1\r\nHost: localhost\r\n\r\nGET /probe HTTP/1.1\r\nHost:")
        .await
        .unwrap();
    let mut socket = tokio::io::BufReader::new(socket);
    assert!(read_headers(&mut socket).await.starts_with("HTTP/1.1 200"));
    // Hyper can prefetch this partial next header during the first request.
    // Its unread buffer is private, so this corner may retain the 60s idle
    // bound instead of Go's 10s header bound. It must never remain unbounded.
    advance_http1_clock(Duration::from_secs(61)).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), socket.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    listener.shutdown().await;
}

#[tokio::test]
async fn upload_idle_returns_refusal_and_preserves_receiver_bytes() {
    let server = Arc::new(HttpServer::new(Config::default().validated().unwrap()).unwrap());
    let listener = native::serve(server, NativeKind::H1, None).await;
    let (_, session) = upload_request(listener.address, "POST", "/upload/session", "", b"").await;
    let session: serde_json::Value = serde_json::from_slice(&session).unwrap();
    let id = session["uploadId"].as_str().unwrap();
    let mut socket = TcpStream::connect(listener.address).await.unwrap();
    socket
        .write_all(
            format!(
                "POST /upload?id={id} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\nConnection: close\r\n\r\na"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let (_, checkpoint) = upload_request(
                listener.address,
                "POST",
                &format!("/upload/checkpoint?id={id}"),
                "",
                b"",
            )
            .await;
            let checkpoint: serde_json::Value = serde_json::from_slice(&checkpoint).unwrap();
            if checkpoint["bytes"] == 1 {
                break;
            }
        }
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
    let (_, checkpoint) = upload_request(
        listener.address,
        "POST",
        &format!("/upload/checkpoint?id={id}"),
        "",
        b"",
    )
    .await;
    let checkpoint: serde_json::Value = serde_json::from_slice(&checkpoint).unwrap();
    assert_eq!(checkpoint["bytes"], 1);
    listener.shutdown().await;
}

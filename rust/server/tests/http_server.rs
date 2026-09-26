use graphite_meter_server::{config::Config, http_server::HttpServer};
use http::{Method, Request, StatusCode, header};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

fn request(path: &str) -> Request<()> {
    Request::builder()
        .uri(path)
        .header(header::HOST, "localhost:7246")
        .body(())
        .unwrap()
}

#[tokio::test]
async fn download_body_owns_capacity_and_options_does_not_consume_it() {
    let mut config = Config::default();
    config.limits.operations_per_client = 1;
    config.limits.sessions_per_client = 1;
    let server = HttpServer::new(Arc::new(config)).unwrap();
    let peer: SocketAddr = "127.0.0.1:31000".parse().unwrap();
    let held = server.respond(request("/download?bytes=100"), peer);
    assert_eq!(held.status(), StatusCode::OK);
    assert_eq!(held.headers()[header::CONTENT_LENGTH], "100");

    let refused = server.respond(request("/download"), peer);
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(refused.headers()[header::RETRY_AFTER], "1");
    assert_eq!(refused.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");

    let mut options = request("/download");
    *options.method_mut() = Method::OPTIONS;
    assert_eq!(
        server.respond(options, peer).status(),
        StatusCode::NO_CONTENT
    );
    drop(held);
    assert_eq!(
        server.respond(request("/download?bytes=0"), peer).status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn download_length_preserves_go_parsing_and_head_headers() {
    let server = HttpServer::new(Arc::new(Config::default())).unwrap();
    let peer = "127.0.0.1:31000".parse().unwrap();
    for (query, expected) in [
        ("", 25 * 1024 * 1024),
        ("bytes=0", 0),
        ("bytes=-1", 25 * 1024 * 1024),
        ("bytes=9223372036854775808", 25 * 1024 * 1024),
        ("bytes=9223372036854775807", 64_u64 * 1024 * 1024 * 1024),
        ("bytes=%2B123", 123),
        ("bytes=5&bytes=10", 5),
    ] {
        let mut request = request(&format!("/download?{query}"));
        *request.method_mut() = Method::HEAD;
        let response = server.respond(request, peer);
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            expected.to_string(),
            "{query}"
        );
        assert!(hyper::body::Body::is_end_stream(response.body()));
    }
}

#[tokio::test]
async fn real_http1_serves_discovery_and_streams_exact_download_then_joins_shutdown() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let serving = tokio::spawn(server.serve_http1(listener, async {
            let _ = stopped.await;
        }));

        for path in ["/preflight", "/servers", "/probe", "/download?bytes=300000"] {
            let mut socket = TcpStream::connect(address).await.unwrap();
            socket
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = Vec::new();
            socket.read_to_end(&mut response).await.unwrap();
            let boundary = response
                .windows(4)
                .position(|part| part == b"\r\n\r\n")
                .unwrap();
            let headers = std::str::from_utf8(&response[..boundary]).unwrap();
            assert!(headers.starts_with("HTTP/1.1 200"), "{path}: {headers}");
            let body = &response[boundary + 4..];
            if path.starts_with("/download") {
                assert_eq!(body.len(), 300000);
                assert_eq!(&body[..37856], &body[262144..]);
                assert!(body.iter().any(|byte| *byte != 0));
            } else {
                let _: serde_json::Value = serde_json::from_slice(body).unwrap();
            }
        }
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
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
        let server = Arc::new(HttpServer::new(Arc::new(config)).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let serving = tokio::spawn(server.clone().serve_http1(listener, async {
            let _ = stopped.await;
        }));
        let mut stalled = TcpStream::connect(address).await.unwrap();
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
        let peer = "127.0.0.1:31000".parse().unwrap();
        assert_eq!(
            server.respond(request("/download?bytes=1"), peer).status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        loop {
            let probe = fetch(address, "/probe").await;
            let boundary = probe
                .windows(4)
                .position(|part| part == b"\r\n\r\n")
                .unwrap();
            let document: serde_json::Value =
                serde_json::from_slice(&probe[boundary + 4..]).unwrap();
            if document["load"]["active"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            server.respond(request("/download?bytes=1"), peer).status(),
            StatusCode::OK
        );
        // Keep the non-reading peer alive until after recovery is observed.
        drop(stalled);
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    })
    .await
    .expect("stalled download retained its admission slot");
}

#[tokio::test]
async fn oversized_http1_headers_are_rejected() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let serving = tokio::spawn(server.serve_http1(listener, async {
            let _ = stopped.await;
        }));
        let mut socket = TcpStream::connect(address).await.unwrap();
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
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    })
    .await
    .expect("oversized header request stalled");
}

async fn fetch(address: SocketAddr, path: &str) -> Vec<u8> {
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
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
        let server = Arc::new(HttpServer::new(Arc::new(config)).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let serving = tokio::spawn(server.serve_http1(listener, async { let _ = stopped.await; }));
        let (headers, session) = upload_request(address, "POST", "/upload/session", "192.0.2.1", b"").await;
        assert!(headers.starts_with("HTTP/1.1 200"));
        assert!(headers.contains("access-control-allow-origin: *"));
        let session: serde_json::Value = serde_json::from_slice(&session).unwrap();
        let id = session["uploadId"].as_str().unwrap();
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket.write_all(format!("GET /upload/progress?id={id} HTTP/1.1\r\nHost: localhost\r\nX-Real-IP: 192.0.2.1\r\n\r\n").as_bytes()).await.unwrap();
        let mut progress = tokio::io::BufReader::new(socket);
        let headers = read_headers(&mut progress).await;
        assert!(headers.contains("application/x-ndjson"));
        assert!(headers.contains("no-store, no-transform"));
        assert!(headers.contains("x-accel-buffering: no"));
        assert_eq!(progress_event(&mut progress).await["type"], "ready");

        let path = format!("/upload?id={id}");
        let first = vec![1; 1234];
        let second = vec![2; 5678];
        let (rejected, _) = upload_request(address, "PUT", &path, "192.0.2.1", &second).await;
        assert!(rejected.starts_with("HTTP/1.1 405"));
        assert!(rejected.contains("allow: POST"));
        let (first, second) = tokio::join!(
            upload_request(address, "POST", &path, "192.0.2.1", &first),
            upload_request(address, "POST", &path, "192.0.2.1", &second),
        );
        for (reply, expected) in [(first, 1234), (second, 5678)] {
            assert!(reply.0.starts_with("HTTP/1.1 200"));
            let value: serde_json::Value = serde_json::from_slice(&reply.1).unwrap();
            assert_eq!(value["bytes"], expected);
        }
        let checkpoint = format!("/upload/checkpoint?id={id}");
        let (headers, bytes) = upload_request(address, "POST", &checkpoint, "192.0.2.1", b"").await;
        assert!(headers.starts_with("HTTP/1.1 200"));
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["bytes"], 6912);
        assert!(value["nanos"].as_u64().unwrap() > 0);
        let (headers, _) = upload_request(address, "POST", &checkpoint, "192.0.2.2", b"").await;
        assert!(headers.starts_with("HTTP/1.1 403"));
        assert!(headers.contains("x-graphite-upload-refusal: ownerMismatch"));
        let (headers, _) = upload_request(address, "POST", "/upload?id=invalid", "192.0.2.1", b"bad").await;
        assert!(headers.starts_with("HTTP/1.1 400"));
        assert!(headers.contains("x-graphite-upload-refusal: invalid"));
        let (headers, body) = upload_request(address, "GET", "/upload/session", "192.0.2.1", b"").await;
        assert!(headers.starts_with("HTTP/1.1 405"));
        assert!(body.is_empty());

        let (headers, body) = upload_request(address, "DELETE", &format!("/upload/progress?id={id}"), "192.0.2.1", b"").await;
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
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }).await.expect("upload lifecycle stalled");
}

#[tokio::test]
async fn stalled_upload_read_releases_capacity_and_keeps_received_bytes() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let config = Config { max_operation_duration: Duration::from_millis(200), ..Config::default() };
        let server = Arc::new(HttpServer::new(Arc::new(config)).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let serving = tokio::spawn(server.serve_http1(listener, async { let _ = stopped.await; }));
        let (_, session) = upload_request(address, "POST", "/upload/session", "", b"").await;
        let session: serde_json::Value = serde_json::from_slice(&session).unwrap();
        let id = session["uploadId"].as_str().unwrap();
        let mut stalled = TcpStream::connect(address).await.unwrap();
        stalled.write_all(format!("POST /upload?id={id} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100000\r\n\r\nabc").as_bytes()).await.unwrap();
        loop {
            let (_, checkpoint) = upload_request(address, "POST", &format!("/upload/checkpoint?id={id}"), "", b"").await;
            if serde_json::from_slice::<serde_json::Value>(&checkpoint).is_ok_and(|value| value["bytes"] == 3) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        loop {
            let (_, probe) = upload_request(address, "GET", "/probe", "", b"").await;
            let probe: serde_json::Value = serde_json::from_slice(&probe).unwrap();
            if probe["load"]["active"] == 0 { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let (_, checkpoint) = upload_request(address, "POST", &format!("/upload/checkpoint?id={id}"), "", b"").await;
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&checkpoint).unwrap()["bytes"], 3);
        drop(stalled);
        stop.send(()).unwrap();
        serving.await.unwrap().unwrap();
    }).await.expect("upload read deadline failed to release capacity");
}

async fn upload_request(
    address: SocketAddr,
    method: &str,
    path: &str,
    owner: &str,
    body: &[u8],
) -> (String, Vec<u8>) {
    let mut socket = TcpStream::connect(address).await.unwrap();
    let owner = if owner.is_empty() {
        String::new()
    } else {
        format!("X-Real-IP: {owner}\r\n")
    };
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n{owner}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
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

#[tokio::test]
async fn progress_claim_cancellation_owns_capacity_and_options_stays_unmetered() {
    use hyper::body::Body;
    use std::{future::poll_fn, pin::Pin};
    let mut config = Config::default();
    config.limits.operations_per_client = 2;
    config.limits.sessions_per_client = 1;
    let server = HttpServer::new(Arc::new(config)).unwrap();
    let peer = "[2001:db8:1::1]:31000".parse().unwrap();
    let neighbor = "[2001:db8:1::2]:31000".parse().unwrap();
    let foreign = "[2001:db8:2::1]:31000".parse().unwrap();
    let mut mint = request("/upload/session");
    *mint.method_mut() = Method::POST;
    let mut minted = server.respond(mint, peer);
    let data = poll_fn(|cx| Pin::new(minted.body_mut()).poll_frame(cx))
        .await
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&data).unwrap();
    let id = value["uploadId"].as_str().unwrap();
    let path = format!("/upload/progress?id={id}");
    let mut first = server.respond(request(&path), peer);
    let ready = poll_fn(|cx| Pin::new(first.body_mut()).poll_frame(cx))
        .await
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&ready).unwrap()["type"],
        "ready"
    );
    let second = server.respond(request(&path), neighbor);
    assert_eq!(
        second.status(),
        StatusCode::OK,
        "same IPv6 /64 shares upload ownership"
    );
    assert_eq!(
        server.respond(request("/download?bytes=1"), peer).status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    for path in [
        "/upload",
        "/upload/session",
        "/upload/checkpoint",
        "/upload/progress",
    ] {
        let mut options = request(path);
        *options.method_mut() = Method::OPTIONS;
        assert_eq!(
            server.respond(options, peer).status(),
            StatusCode::NO_CONTENT
        );
    }
    let mut checkpoint = request(&format!("/upload/checkpoint?id={id}"));
    *checkpoint.method_mut() = Method::POST;
    let refused = server.respond(checkpoint, foreign);
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        refused.headers()["x-graphite-upload-refusal"],
        "ownerMismatch"
    );
    assert!(
        poll_fn(|cx| Pin::new(first.body_mut()).poll_frame(cx))
            .await
            .is_none()
    );
    drop(first);
    assert_eq!(
        server.respond(request("/download?bytes=1"), peer).status(),
        StatusCode::OK
    );
    drop(second);
}

async fn advance_http1_clock(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
}

#[tokio::test]
async fn keepalive_idle_uses_sixty_seconds_and_releases_connection_capacity() {
    let server = Arc::new(
        HttpServer::new(Arc::new(Config {
            max_connections: 1,
            max_connections_per_client: 1,
            ..Config::default()
        }))
        .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let serving = tokio::spawn(server.serve_http1(listener, async {
        let _ = stopped.await;
    }));
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(b"GET /download?bytes=0 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut socket = tokio::io::BufReader::new(socket);
    assert!(read_headers(&mut socket).await.starts_with("HTTP/1.1 200"));
    advance_http1_clock(Duration::from_secs(59)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), socket.read(&mut [0; 1]))
            .await
            .is_err(),
        "keepalive closed before 60 seconds"
    );
    let mut rejected = TcpStream::connect(address).await.unwrap();
    assert_eq!(rejected.read(&mut [0; 1]).await.unwrap(), 0);
    advance_http1_clock(Duration::from_secs(2)).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), socket.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let reply = fetch(address, "/probe").await;
    assert!(reply.starts_with(b"HTTP/1.1 200"));
    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn partial_headers_keep_the_ten_second_bound_after_keepalive_idle() {
    let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let serving = tokio::spawn(server.serve_http1(listener, async {
        let _ = stopped.await;
    }));
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(b"GET /download?bytes=0 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut socket = tokio::io::BufReader::new(socket);
    read_headers(&mut socket).await;
    advance_http1_clock(Duration::from_secs(20)).await;
    socket
        .get_mut()
        .write_all(b"GET /probe HTTP/1.1\r\nHost:")
        .await
        .unwrap();
    // Let the actual socket read establish the partial-header deadline.
    tokio::time::sleep(Duration::from_millis(10)).await;
    advance_http1_clock(Duration::from_secs(11)).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), socket.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn active_http1_progress_survives_an_idle_interval() {
    let server = Arc::new(
        HttpServer::new(Arc::new(Config {
            max_operation_duration: Duration::from_secs(180),
            ..Config::default()
        }))
        .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let serving = tokio::spawn(server.serve_http1(listener, async {
        let _ = stopped.await;
    }));
    let (_, session) = upload_request(address, "POST", "/upload/session", "", b"").await;
    let session: serde_json::Value = serde_json::from_slice(&session).unwrap();
    let id = session["uploadId"].as_str().unwrap();
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(
            format!("GET /upload/progress?id={id} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut progress = tokio::io::BufReader::new(socket);
    read_headers(&mut progress).await;
    assert_eq!(progress_event(&mut progress).await["type"], "ready");
    advance_http1_clock(Duration::from_secs(61)).await;
    let (headers, _) = upload_request(
        address,
        "DELETE",
        &format!("/upload/progress?id={id}"),
        "",
        b"",
    )
    .await;
    assert!(headers.starts_with("HTTP/1.1 204"));
    assert_eq!(progress_event(&mut progress).await["type"], "complete");
    drop(progress);
    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn prefetched_partial_pipeline_still_has_a_finite_idle_bound() {
    let server = Arc::new(HttpServer::new(Arc::new(Config::default())).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let serving = tokio::spawn(server.serve_http1(listener, async {
        let _ = stopped.await;
    }));
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket.write_all(b"GET /download?bytes=0 HTTP/1.1\r\nHost: localhost\r\n\r\nGET /probe HTTP/1.1\r\nHost:").await.unwrap();
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
    stop.send(()).unwrap();
    serving.await.unwrap().unwrap();
}

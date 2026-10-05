//! What bounds an HTTP/1 connection: keep-alive idleness, the exchange bound, head limits and connection shares.

use super::*;
use tokio::time::{Instant, sleep};

/// How long the server takes to close the connection, on paused time.
async fn closes_after(client: &mut Client<TcpStream>) -> Duration {
    tokio::time::pause();
    let started = Instant::now();
    assert!(client.answer().await.is_none(), "the server closed the connection");
    started.elapsed()
}

#[tokio::test]
async fn an_idle_keep_alive_connection_closes_after_fifteen_seconds() {
    let server = start(&[]).await;
    let mut client = server.connect().await;
    assert_eq!(client.request("GET /probe", "").await.status, 200);
    let elapsed = closes_after(&mut client).await;
    assert!(
        elapsed > Duration::from_secs(14) && elapsed < Duration::from_millis(15_100),
        "{elapsed:?}"
    );
}

/// Sends part of a head after `idle` seconds without traffic, and returns how long the server then took to close.
async fn partial_head_closes_after(client: &mut Client<TcpStream>, idle: u64) -> Duration {
    tokio::time::pause();
    sleep(Duration::from_secs(idle)).await;
    tokio::time::resume();
    client.send("GET /probe HTTP/1.1\r\nHost: test\r\n").await;
    sleep(Duration::from_millis(50)).await;
    closes_after(client).await
}

#[tokio::test]
async fn the_first_request_has_fifteen_seconds_from_the_connection_start() {
    let server = start(&[]).await;
    let elapsed = partial_head_closes_after(&mut server.connect().await, 10).await;
    assert!(elapsed > Duration::from_secs(4) && elapsed < Duration::from_millis(5_100), "{elapsed:?}");
}

#[tokio::test]
async fn a_later_request_head_has_fifteen_seconds_from_its_first_byte() {
    let server = start(&[]).await;
    let mut client = server.connect().await;
    assert_eq!(client.request("GET /probe", "").await.status, 200);
    let elapsed = partial_head_closes_after(&mut client, 10).await;
    assert!(
        elapsed > Duration::from_secs(14) && elapsed < Duration::from_millis(15_100),
        "{elapsed:?}"
    );
}

#[tokio::test]
async fn an_unadmitted_reply_the_peer_does_not_read_is_cut_at_the_exchange_bound() {
    const REQUESTS: usize = 40_000;
    let server = start(&[]).await;
    let reply = server.get("/probe").await.body.len();
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let (mut reader, mut writer) = socket.connect(server.address).await.unwrap().into_split();
    let requests = "GET /probe HTTP/1.1\r\nHost: test\r\n\r\n".repeat(REQUESTS);
    let sending = tokio::spawn(async move {
        let _ = writer.write_all(requests.as_bytes()).await;
        std::future::pending::<()>().await;
        writer
    });
    sleep(Duration::from_millis(200)).await;
    tokio::time::pause();
    sleep(Duration::from_secs(16)).await;
    tokio::time::resume();
    let (mut buffer, mut received) = (vec![0; 64 << 10], 0);
    while let Ok(read @ 1..) = reader.read(&mut buffer).await {
        received += read;
    }
    sending.abort();
    assert!(received < REQUESTS * reply / 2, "the replies were cut with {received} bytes read");
}

#[tokio::test]
async fn a_head_over_thirty_two_kibibytes_is_refused() {
    let server = start(&[]).await;
    let mut client = server.connect().await;
    let field = |bytes| "a".repeat(bytes);
    let head = |bytes| format!("GET /probe HTTP/1.1\r\nHost: test\r\nX-Pad: {}\r\n\r\n", field(bytes));
    client.send(&head(30 << 10)).await;
    assert_eq!(client.answer().await.unwrap().status, 200);
    client.send(&head(33 << 10)).await;
    assert_eq!(client.answer().await.unwrap().status, 431);
}

#[tokio::test]
async fn a_refused_head_or_body_ends_the_connection() {
    let server = start(&[]).await;
    for (request, text) in [
        ("GET /probe HTTP/1.1\r\n\r\n", "400 Bad Request: missing required Host header"),
        (
            "GET /probe HTTP/1.1\r\nHost: test\r\nContent-Length: 3\r\n\r\nabc",
            "request body not accepted",
        ),
    ] {
        let mut client = server.connect().await;
        client.send(request).await;
        let answer = client.answer().await.unwrap();
        assert_eq!(answer.status, 400);
        assert_eq!(answer.header("connection"), Some("close"));
        assert_eq!(answer.body, format!("{text}\n").as_bytes());
        assert!(client.answer().await.is_none());
    }
}

#[tokio::test]
async fn connections_beyond_a_client_share_are_closed_unserved() {
    let server = start(&[("GM_MAX_CONNECTIONS_PER_CLIENT", "2")]).await;
    let mut held = Vec::new();
    for _ in 0..2 {
        let mut client = server.connect().await;
        assert_eq!(client.request("GET /probe", "").await.status, 200);
        held.push(client);
    }
    let mut refused = server.connect().await;
    refused.send("GET /probe HTTP/1.1\r\nHost: test\r\n\r\n").await;
    assert!(refused.answer().await.is_none());
    drop(held);
    sleep(Duration::from_millis(50)).await;
    assert_eq!(server.get("/probe").await.status, 200, "closed connections release their shares");
}

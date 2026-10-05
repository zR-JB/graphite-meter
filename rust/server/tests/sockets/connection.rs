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

#[tokio::test]
async fn a_request_head_has_fifteen_seconds_from_its_first_byte() {
    let server = start(&[]).await;
    let mut client = server.connect().await;
    tokio::time::pause();
    sleep(Duration::from_secs(10)).await;
    tokio::time::resume();
    client.send("GET /probe HTTP/1.1\r\nHost: test\r\n").await;
    sleep(Duration::from_millis(50)).await;
    let elapsed = closes_after(&mut client).await;
    assert!(
        elapsed > Duration::from_secs(14) && elapsed < Duration::from_millis(15_100),
        "{elapsed:?}"
    );
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

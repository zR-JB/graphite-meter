//! What bounds an HTTP/1 connection: keep-alive idleness, the exchange bound, head limits and connection shares.

use super::*;
use futures_util::FutureExt;
use tokio::time::{Instant, sleep};

/// How long the server takes to close the connection, on paused time moved in steps, so that no timer the close
/// starts moves it further.
async fn closes_after(client: &mut Client<TcpStream>) -> Duration {
    tokio::time::pause();
    let started = Instant::now();
    loop {
        tokio::time::advance(Duration::from_millis(50)).await;
        tokio::task::yield_now().await;
        // The close reaches the client's socket in real time.
        std::thread::sleep(Duration::from_millis(1));
        if let Some(answer) = client.answer().now_or_never() {
            assert!(answer.is_none(), "the server closed the connection");
            tokio::time::resume();
            return started.elapsed();
        }
    }
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
async fn keep_alive_idleness_and_request_heads_have_fifteen_seconds() {
    let server = start(&[]).await;
    let fifteen = |elapsed: Duration| elapsed > Duration::from_secs(14) && elapsed < Duration::from_millis(15_100);
    let mut idle = server.connect().await;
    assert_eq!(idle.request("GET /probe", "").await.status, 200);
    let elapsed = closes_after(&mut idle).await;
    assert!(fifteen(elapsed), "an idle keep-alive connection: {elapsed:?}");
    let elapsed = partial_head_closes_after(&mut server.connect().await, 10).await;
    let five = elapsed > Duration::from_secs(4) && elapsed < Duration::from_millis(5_100);
    assert!(five, "the first head counts from the connection start: {elapsed:?}");
    let mut later = server.connect().await;
    assert_eq!(later.request("GET /probe", "").await.status, 200);
    let elapsed = partial_head_closes_after(&mut later, 10).await;
    assert!(fifteen(elapsed), "a later head counts from its first byte: {elapsed:?}");
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

#[tokio::test]
async fn discovery_names_the_bound_port_of_a_listener_configured_with_port_zero() {
    let server = start(&[]).await;
    let port = server.address.port();
    let preflight: serde_json::Value = serde_json::from_slice(&server.get("/preflight").await.body).unwrap();
    let target = &preflight["capabilities"]["throughput"][0];
    assert_eq!(target["baseUrl"], format!("http://test:{port}"));
    let servers = server.get("/servers").await;
    assert_eq!(servers.status, 200);
    let page = server.get("/").await;
    let policy = page.header("content-security-policy").unwrap();
    assert!(policy.contains(&format!("http://test:{port}")), "{policy}");
}

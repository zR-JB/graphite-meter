//! The Linux endpoint set: HTTP/3 on half the runtime threads, migrating clients and server-wide limits.
#![cfg(target_os = "linux")]

use super::{
    http3::{Connection, H3, read, transport},
    quic::retried,
};
use futures_util::future::join_all;
use std::net::SocketAddr;

fn endpoints(h3: &H3) -> usize {
    let Some(SocketAddr::V4(quic)) = h3.server.quic else {
        panic!("an IPv4 HTTP/3 address")
    };
    let ip = u32::from_ne_bytes(quic.ip().octets());
    let local = format!("{ip:08X}:{:04X}", quic.port());
    let sockets = std::fs::read_to_string("/proc/self/net/udp").unwrap();
    let bound = |line: &&str| line.split_whitespace().nth(1) == Some(local.as_str());
    sockets.lines().filter(bound).count()
}

async fn download(connection: &Connection, bytes: usize) -> usize {
    let (answer, mut body) = connection
        .send("GET", &format!("/download?bytes={bytes}"), b"")
        .await;
    assert_eq!(answer.status(), 200);
    read(&mut body).await.unwrap().len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_workers_run_two_endpoints_that_follow_rebound_clients() {
    let h3 = H3::start(&[]).await;
    assert_eq!(endpoints(&h3), 2);
    // Distinct sources stand for clients whose 4-tuples the kernel spreads over the endpoints.
    let transfers = (2..10).map(|source| {
        let h3 = &h3;
        async move {
            let connection = h3
                .connect_from(&format!("127.0.0.{source}"), transport(None))
                .await;
            download(&connection.unwrap(), 1 << 20).await
        }
    });
    assert_eq!(join_all(transfers).await, [1 << 20; 8]);
    let connection = h3.connect(transport(None)).await;
    assert_eq!(download(&connection, 13).await, 13);
    // Each new port reaches the other endpoint one time in two, whose socket forwards to the connection's.
    for rebind in 1..=16 {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        connection.endpoint.rebind(socket).unwrap();
        assert_eq!(
            download(&connection, 64 << 10).await,
            64 << 10,
            "after rebind {rebind}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_source_s_handshake_in_progress_needs_retry_on_every_endpoint() {
    let h3 = H3::start(&[]).await;
    assert_eq!(endpoints(&h3), 2);
    // The first Initial's handshake waits for a flight that never comes.
    assert!(!retried(&h3, "127.0.0.3").await);
    // Each handshake's new port reaches either endpoint.
    for _ in 0..16 {
        assert!(
            retried(&h3, "127.0.0.3").await,
            "a handshake from the source is in progress on one endpoint"
        );
    }
    let mut held = Vec::new();
    for _ in 0..12 {
        let connection = h3.connect_from("127.0.0.4", transport(None)).await.unwrap();
        // An answer means the server's side of the handshake completed too.
        assert_eq!(download(&connection, 13).await, 13);
        held.push(connection);
    }
    assert!(
        !retried(&h3, "127.0.0.4").await,
        "connections the source holds ask no Retry"
    );
}

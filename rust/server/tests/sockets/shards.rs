//! The Linux endpoint set: HTTP/3 on half of the runtime threads, connections that follow a client whose address
//! changes, and limits that stay server-wide.
#![cfg(target_os = "linux")]

use super::{
    http3::{Connection, H3, read, transport},
    quic::retried,
};
use futures_util::future::join_all;
use graphite_meter_server::limits::QUIC_PER_CLIENT;

async fn download(connection: &Connection, bytes: usize) -> usize {
    let (answer, mut body) = connection.send("GET", &format!("/download?bytes={bytes}"), b"").await;
    assert_eq!(answer.status(), 200);
    read(&mut body).await.unwrap().len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_workers_run_two_endpoints_that_follow_rebound_clients() {
    let h3 = H3::start(&[]).await;
    assert_eq!(h3.server.endpoints, 2);
    // Distinct sources stand for clients whose 4-tuples the kernel spreads over the endpoints.
    let transfers = (2..10).map(|source| {
        let h3 = &h3;
        async move {
            let connection = h3.connect_from(&format!("127.0.0.{source}"), transport(None)).await;
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
        assert_eq!(download(&connection, 64 << 10).await, 64 << 10, "after rebind {rebind}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_and_a_source_s_quic_share_hold_across_endpoints() {
    let h3 = H3::start(&[]).await;
    assert_eq!(h3.server.endpoints, 2);
    let mut held = vec![h3.connect_from("127.0.0.3", transport(None)).await.unwrap()];
    // Each handshake's new port reaches either endpoint.
    for _ in 0..16 {
        assert!(retried(&h3, "127.0.0.3").await, "the source holds a connection on one endpoint");
    }
    for _ in 1..QUIC_PER_CLIENT {
        held.push(h3.connect_from("127.0.0.3", transport(None)).await.unwrap());
    }
    let refused = h3.connect_from("127.0.0.3", transport(None)).await;
    assert!(refused.is_err(), "a source's QUIC share counts every endpoint's connections");
}

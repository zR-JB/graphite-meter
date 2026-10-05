//! The Linux endpoint set: HTTP/3 on half of the runtime threads as far as the budget covers, connections that
//! follow a client whose address changes, and limits that stay server-wide.
#![cfg(target_os = "linux")]

use super::{
    http3::{ADDRESS, Connection, H3, read, transport},
    quic::retried,
    *,
};
use futures_util::future::join_all;
use graphite_meter_server::limits::QUIC_PER_CLIENT;
use graphite_meter_testkit::{Identity, Scratch};

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

/// A server with HTTP/3 on the chain in `scratch`, four connections and a budget of `budget` bytes, if it binds.
async fn bind(scratch: &Scratch, budget: Option<usize>) -> Result<Server, String> {
    let [cert, key] = ["cert.pem", "key.pem"].map(|name| scratch.path().join(name));
    let budget = budget.map(|budget| budget.to_string());
    let mut env = vec![
        ("GM_TLS_CERT", cert.to_str().unwrap()),
        ("GM_TLS_KEY", key.to_str().unwrap()),
        ("GM_H3_ADDR", ADDRESS),
        ("GM_MAX_CONNECTIONS", "4"),
        ("GM_MAX_CONNECTIONS_PER_CLIENT", "4"),
    ];
    env.extend(budget.as_deref().map(|budget| ("GM_MAX_BUFFER_BYTES", budget)));
    Server::bind(config(&env)).await
}

/// The number before `suffix` in a budget refusal.
fn term(refusal: &str, suffix: &str) -> usize {
    let before = refusal.split(suffix).next().unwrap();
    before.rsplit(' ').next().unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn the_buffer_budget_caps_the_endpoints() {
    let (scratch, identity) = (Scratch::new().unwrap(), Identity::generate().unwrap());
    scratch.file("cert.pem", &identity.certificate).unwrap();
    scratch.file("key.pem", &identity.key).unwrap();
    let measured = bind(&scratch, None).await.unwrap();
    assert_eq!(measured.quic_endpoints(), 4, "eight workers plan four endpoints");
    let reserved = measured.budget().usage().reserved;
    drop(measured);
    let refusal = bind(&scratch, Some(1)).await.err().unwrap();
    let minimum = term(&refusal, ": GM_MAX_CONNECTIONS");
    // Every connection's floor and the download block, beside the configured endpoint's buffers.
    let rest = minimum - term(&refusal, " bytes of QUIC endpoint buffers");
    let four = bind(&scratch, Some(rest + reserved)).await.unwrap();
    assert_eq!(four.quic_endpoints(), 4);
    drop(four);
    let three = bind(&scratch, Some(rest + reserved - 1)).await.unwrap();
    assert_eq!(three.quic_endpoints(), 3, "a byte short of four endpoints");
}

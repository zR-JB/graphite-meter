//! The QUIC endpoint: Retry under pressure, the handshake bound, receive credit, and the budget it binds within.

use super::{
    http3::{ADDRESS, H3, read, transport},
    *,
};
use graphite_meter_server::limits::CONNECTION_CREDIT;
use graphite_meter_testkit::{Identity, Scratch};
use tokio::net::UdpSocket;

const INITIAL: u8 = 0;
const RETRY: u8 = 3;

/// A client's first Initial towards the server, caught on a socket that never answers.
async fn initial(h3: &H3) -> Vec<u8> {
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let endpoint = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let connecting = endpoint.connect_with(h3.client(transport(None)), silent.local_addr().unwrap(), "localhost");
    let mut packet = vec![0; 2048];
    let length = silent.recv(&mut packet).await.unwrap();
    drop(connecting);
    packet.truncate(length);
    packet
}

/// Whether the server answers an Initial from the local address `source` with a Retry rather than its own Initial.
async fn retried(h3: &H3, source: &str) -> bool {
    let socket = UdpSocket::bind(format!("{source}:0")).await.unwrap();
    socket
        .send_to(&initial(h3).await, h3.server.quic.unwrap())
        .await
        .unwrap();
    let mut packet = [0; 2048];
    tokio::time::timeout(Duration::from_secs(2), socket.recv(&mut packet))
        .await
        .unwrap()
        .unwrap();
    // A long header names its type in bits 4 and 5; the fixed bit beside them may be greased.
    assert_ne!(packet[0] & 0x80, 0, "a long header");
    match packet[0] >> 4 & 0x3 {
        RETRY => true,
        INITIAL => false,
        kind => panic!("a handshake answer, not type {kind}"),
    }
}

#[tokio::test]
async fn unvalidated_handshakes_need_retry_under_pressure_or_from_a_source_holding_a_connection() {
    let h3 = H3::start(&[]).await;
    assert!(!retried(&h3, "127.0.0.2").await);
    let _held = h3.connect_from("127.0.0.3", transport(None)).await.unwrap();
    assert!(retried(&h3, "127.0.0.3").await, "the source holds a connection");
    assert!(!retried(&h3, "127.0.0.4").await);
    let budget = h3.server.budget.clone();
    let quarter = budget.lease(budget.usage().limit / 4).unwrap();
    assert!(retried(&h3, "127.0.0.5").await, "a quarter of the budget is used");
    drop(quarter);

    let limits = [("GM_MAX_CONNECTIONS", "4"), ("GM_MAX_CONNECTIONS_PER_CLIENT", "4")];
    let h3 = H3::start(&limits).await;
    assert!(!retried(&h3, "127.0.0.2").await, "the handshake holds one of four connections");
    assert!(retried(&h3, "127.0.0.3").await, "a quarter of the connections are used");
}

#[tokio::test]
async fn a_silent_handshake_holds_its_connection_for_ten_seconds() {
    let h3 = H3::start(&[("GM_MAX_CONNECTIONS_PER_CLIENT", "1")]).await;
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    silent
        .send_to(&initial(&h3).await, h3.server.quic.unwrap())
        .await
        .unwrap();
    silent.recv(&mut [0; 2048]).await.unwrap();
    let refused = h3.connect_from("127.0.0.1", transport(None)).await.err();
    assert!(refused.is_some(), "the handshake holds the client's only connection");
    for _ in 0..9 {
        advance_clock(Duration::from_secs(1)).await;
    }
    assert!(h3.connect_from("127.0.0.1", transport(None)).await.is_err(), "nine seconds in");
    advance_clock(Duration::from_millis(1500)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        h3.connect_from("127.0.0.1", transport(None)).await.is_ok(),
        "the handshake ended at ten seconds"
    );
}

/// The budget's usage once it held still for a while.
async fn settled(budget: &Budget) -> usize {
    let mut last = budget.usage().used;
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let used = budget.usage().used;
        if used == last {
            return used;
        }
        last = used;
    }
}

#[tokio::test]
async fn only_an_admitted_upload_reserves_receive_credit_until_its_connection_is_gone() {
    let env = [("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "1"), ("GM_MAX_SESSIONS_PER_CLIENT", "1")];
    let h3 = H3::start(&env).await;
    let budget = h3.server.budget.clone();
    let idle = settled(&budget).await;
    let connection = h3.connect(transport(None)).await;
    let id = h3.server.upload_id().await;
    let (_download, mut download) = connection.send("GET", ENDLESS, b"").await;
    let (refused, _) = connection.send("POST", &format!("/upload?id={id}"), b"refused").await;
    assert_eq!(refused.status(), 429, "the client's share is held");
    let unfunded = settled(&budget).await - idle;
    assert!(unfunded < CONNECTION_CREDIT / 4, "{unfunded} bytes without an admitted upload");
    download.stop(graphite_meter_http3::Code::H3_REQUEST_CANCELLED);
    h3.server.until_active(0).await;
    let mut answer = connection.send("POST", &format!("/upload?id={id}"), b"funded").await.1;
    assert_eq!(read(&mut answer).await.unwrap(), br#"{"bytes":6}"#);
    let funded = settled(&budget).await - idle;
    assert!(funded >= CONNECTION_CREDIT, "{funded} bytes once an admitted upload read");
    connection.quic.close(0_u32.into(), b"done");
    let drained = async {
        while budget.usage().used != idle {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), drained)
        .await
        .expect("the budget drains to its baseline");
}

/// Why a server with HTTP/3, two connections and a budget of `budget` bytes does not bind, if it does not.
async fn refusal(scratch: &Scratch, budget: &str) -> Option<String> {
    let [cert, key] = ["cert.pem", "key.pem"].map(|name| scratch.path().join(name));
    let env = [
        ("GM_TLS_CERT", cert.to_str().unwrap()),
        ("GM_TLS_KEY", key.to_str().unwrap()),
        ("GM_H3_ADDR", ADDRESS),
        ("GM_MAX_CONNECTIONS", "2"),
        ("GM_MAX_CONNECTIONS_PER_CLIENT", "2"),
        ("GM_MAX_BUFFER_BYTES", budget),
    ];
    Server::bind(config(&env)).await.err()
}

/// The least budget a refusal names.
fn minimum(refusal: &str) -> u64 {
    let minimum = refusal.split("must be at least ").nth(1).unwrap();
    minimum.split(':').next().unwrap().parse().unwrap()
}

#[tokio::test]
async fn binding_checks_the_budget_with_the_chain_and_the_socket_it_holds() {
    let (scratch, identity) = (Scratch::new().unwrap(), Identity::generate().unwrap());
    scratch.file("cert.pem", &identity.certificate).unwrap();
    scratch.file("key.pem", &identity.key).unwrap();
    let configured = refusal(&scratch, "1").await.unwrap();
    assert!(!configured.contains(" 0 bytes of QUIC endpoint buffers"), "{configured}");
    let socket = refusal(&scratch, &minimum(&configured).to_string()).await;
    let socket = socket.expect("the socket's buffers count");
    assert!(minimum(&socket) > minimum(&configured), "{socket}");
    assert_eq!(refusal(&scratch, &minimum(&socket).to_string()).await, None);
}

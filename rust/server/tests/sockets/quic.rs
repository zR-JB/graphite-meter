//! The QUIC endpoint: Retry under pressure, the handshake bound, receive credit and an exhausted budget.

use super::{
    http3::{H3, read, transport},
    *,
};
use graphite_meter_server::limits::CONNECTION_CREDIT;
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
pub(super) async fn retried(h3: &H3, source: &str) -> bool {
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
pub(super) async fn settled(budget: &Budget) -> usize {
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

#[tokio::test]
async fn a_peer_sending_on_streams_nothing_reads_holds_at_most_192_kib() {
    let h3 = H3::start(&[]).await;
    let budget = h3.server.budget.clone();
    let connection = h3.connect(transport(None)).await;
    // The ping bus accepts no stream, so their data waits unread.
    let session = connection.session("/wt/ping").await;
    h3.server.until_active(1).await;
    let (before, sent) = (settled(&budget).await, connection.quic.stats().udp_tx.bytes);
    let chunk = bytes::Bytes::from(vec![7; 16 << 10]);
    for _ in 0..2 {
        let mut stream = session.open_uni().await.unwrap();
        let sending = async { while stream.write_chunk(chunk.clone()).await.is_ok() {} };
        let _ = tokio::time::timeout(Duration::from_millis(500), sending).await;
    }
    let sent = connection.quic.stats().udp_tx.bytes - sent;
    let held = settled(&budget).await.saturating_sub(before);
    assert!(sent <= 192 << 10 && held <= 192 << 10, "{sent} bytes sent, {held} held");
}

#[tokio::test]
async fn under_pressure_an_admitted_upload_reserves_no_receive_credit() {
    let h3 = H3::start(&[]).await;
    let budget = h3.server.budget.clone();
    let connection = h3.connect(transport(None)).await;
    let id = connection.upload_id().await;
    let pressure = budget.lease(budget.usage().limit / 4 * 3).unwrap();
    let before = settled(&budget).await;
    let mut answer = connection
        .send("POST", &format!("/upload?id={id}"), b"held back")
        .await
        .1;
    assert_eq!(read(&mut answer).await.unwrap(), br#"{"bytes":9}"#);
    let held = settled(&budget).await.saturating_sub(before);
    assert!(held < CONNECTION_CREDIT / 4, "{held} bytes under pressure");
    drop(pressure);
}

/// Leases what the budget has left past `spare` bytes.
fn exhaust(budget: &Budget, spare: usize) -> graphite_meter_server::limits::Lease {
    loop {
        let left = budget.usage().limit - budget.usage().used;
        if let Some(lease) = budget.lease(left.saturating_sub(spare)) {
            return lease;
        }
    }
}

#[tokio::test]
async fn an_exhausted_budget_slows_running_connections_and_closes_one_whose_floor_does_not_fit() {
    let h3 = H3::start(&[]).await;
    let budget = h3.server.budget.clone();
    let running = h3.connect(transport(None)).await;
    let mut download = running.send("GET", "/download?bytes=4194304", b"").await.1;
    let exhausted = exhaust(&budget, 0);
    let bytes = tokio::time::timeout(Duration::from_secs(10), read(&mut download)).await;
    assert_eq!(bytes.unwrap().unwrap().len(), 4 << 20, "refused charges leave the transfer its floors");
    drop(exhausted);

    let short = exhaust(&budget, 128 << 10);
    let closed = match h3.connect_from("127.0.0.2", transport(None)).await {
        Ok(refused) => refused.quic.closed().await,
        Err(error) => error,
    };
    let noq::ConnectionError::ConnectionClosed(close) = closed else {
        panic!("{closed:?}")
    };
    assert_eq!(close.error_code, noq::TransportErrorCode::INTERNAL_ERROR);
    assert_eq!(running.quic.close_reason(), None);
    drop(short);
    assert_eq!(
        running.json("GET", "/probe").await["load"]["active"],
        0,
        "the running connection serves on"
    );
}

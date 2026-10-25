//! The QUIC endpoint: Retry under pressure, the handshake bound, receive credit, late packets and an exhausted budget.

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
    let connecting = endpoint.connect_with(
        h3.client(transport(None)),
        silent.local_addr().unwrap(),
        "localhost",
    );
    let mut packet = vec![0; 2048];
    let length = silent.recv(&mut packet).await.unwrap();
    drop(connecting);
    packet.truncate(length);
    packet
}

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
async fn unvalidated_handshakes_need_retry_under_pressure_or_beside_another_from_their_source() {
    let h3 = H3::start(&[]).await;
    let held = h3.connect_from("127.0.0.2", transport(None)).await.unwrap();
    // An answer means the server's side of the handshake completed too.
    assert_eq!(held.send("GET", "/probe", b"").await.0.status(), 200);
    assert!(
        !retried(&h3, "127.0.0.2").await,
        "the source's connections are no reason"
    );
    assert!(
        retried(&h3, "127.0.0.2").await,
        "the first Initial's handshake is still in progress"
    );
    assert!(!retried(&h3, "127.0.0.4").await);
    let budget = h3.server.budget.clone();
    let quarter = budget.lease(budget.usage().limit / 4).unwrap();
    assert!(
        retried(&h3, "127.0.0.5").await,
        "a quarter of the budget is used"
    );
    drop(quarter);

    let limits = [
        ("GM_MAX_CONNECTIONS", "4"),
        ("GM_MAX_CONNECTIONS_PER_CLIENT", "4"),
    ];
    let h3 = H3::start(&limits).await;
    assert!(
        !retried(&h3, "127.0.0.2").await,
        "the handshake holds one of four connections"
    );
    assert!(
        retried(&h3, "127.0.0.3").await,
        "a quarter of the connections are used"
    );
}

/// A peer's packet after its connection ended gets no stateless reset, which Firefox takes for a close.
#[tokio::test]
async fn a_late_packet_for_an_ended_connection_gets_no_answer() {
    let h3 = H3::start(&[]).await;
    let server = h3.server.quic.unwrap();
    let relay = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let late = Arc::new(std::sync::Mutex::new(Vec::new()));
    let relaying = tokio::spawn({
        let (relay, late) = (relay.clone(), late.clone());
        async move {
            let (mut packet, mut client) = (vec![0; 65536], None);
            loop {
                let Ok((length, from)) = relay.recv_from(&mut packet).await else {
                    continue;
                };
                let datagram = &packet[..length];
                let to = if from == server {
                    client.unwrap()
                } else {
                    // A short header that carried a request is large enough to be answered with a reset.
                    if datagram[0] & 0x80 == 0 && length > 100 {
                        *late.lock().unwrap() = datagram.to_vec();
                    }
                    client = Some(from);
                    server
                };
                let _ = relay.send_to(datagram, to).await;
            }
        }
    });
    let connection = h3
        .connect_via(relay.local_addr().unwrap(), transport(None))
        .await;
    assert_eq!(connection.send("GET", "/probe", b"").await.0.status(), 200);
    connection.quic.close(0_u32.into(), b"done");
    connection.endpoint.wait_idle().await;
    relaying.abort();
    // Past the server's draining period, it no longer knows the connection's IDs.
    advance_clock(Duration::from_secs(5)).await;
    let late = late.lock().unwrap().clone();
    relay.send_to(&late, server).await.unwrap();
    let mut answer = [0; 2048];
    let answered = tokio::time::timeout(Duration::from_millis(300), relay.recv(&mut answer)).await;
    assert!(answered.is_err(), "a late packet is dropped");
}

/// The budget's usage once it held still for three 30 ms windows in a row.
pub(super) async fn settled(budget: &Budget) -> usize {
    let settling = async {
        let (mut last, mut still) = (budget.usage().used, 0);
        while still < 3 {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let used = budget.usage().used;
            still = if used == last { still + 1 } else { 0 };
            last = used;
        }
        last
    };
    tokio::time::timeout(Duration::from_secs(10), settling)
        .await
        .expect("the budget settles")
}

#[tokio::test]
async fn only_admitted_uploads_hold_receive_credit_while_they_read() {
    let env = [
        ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "1"),
        ("GM_MAX_SESSIONS_PER_CLIENT", "1"),
    ];
    let h3 = H3::start(&env).await;
    let budget = h3.server.budget.clone();
    let idle = settled(&budget).await;
    let connection = h3.connect(transport(None)).await;
    let id = h3.server.upload_id().await;
    let (_download, mut download) = connection.send("GET", ENDLESS, b"").await;
    let (refused, _) = connection
        .send("POST", &format!("/upload?id={id}"), b"refused")
        .await;
    assert_eq!(refused.status(), 429, "the client's share is held");
    let unfunded = settled(&budget).await - idle;
    assert!(
        unfunded < CONNECTION_CREDIT / 4,
        "{unfunded} bytes without an admitted upload"
    );
    download.stop(graphite_meter_http3::Code::H3_REQUEST_CANCELLED);
    h3.server.until_active(0).await;
    let (mut upload, mut answer) = connection.open("POST", &format!("/upload?id={id}")).await;
    upload
        .send_data(bytes::Bytes::from_static(b"funded"))
        .await
        .unwrap();
    let funded = settled(&budget).await - idle;
    assert!(
        funded >= CONNECTION_CREDIT,
        "{funded} bytes while an admitted upload reads"
    );
    upload.finish().await.unwrap();
    assert_eq!(answer.response().await.unwrap().status(), 200);
    assert_eq!(read(&mut answer).await.unwrap(), br#"{"bytes":6}"#);
    let returned = settled(&budget).await - idle;
    assert!(
        returned < CONNECTION_CREDIT / 4 && connection.quic.close_reason().is_none(),
        "{returned} bytes once it stopped, on a connection still open"
    );
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
async fn the_last_upload_returns_its_credit_though_the_budget_is_full() {
    let h3 = H3::start(&[]).await;
    let budget = h3.server.budget.clone();
    let idle = settled(&budget).await;
    let connection = h3.connect(transport(None)).await;
    let id = h3.server.upload_id().await;
    let (mut upload, mut answer) = connection.open("POST", &format!("/upload?id={id}")).await;
    upload
        .send_data(bytes::Bytes::from_static(b"funded"))
        .await
        .unwrap();
    assert!(settled(&budget).await - idle >= CONNECTION_CREDIT);
    let _download = connection.send("GET", ENDLESS, b"").await;
    settled(&budget).await;
    let exhausted = exhaust(&budget, 0);
    let full = budget.usage().used;
    upload.finish().await.unwrap();
    assert_eq!(answer.response().await.unwrap().status(), 200);
    assert_eq!(read(&mut answer).await.unwrap(), br#"{"bytes":6}"#);
    let returned = async {
        while budget.usage().used > full - CONNECTION_CREDIT / 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(1), returned)
        .await
        .expect("the credit returns");
    drop(exhausted);
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
    assert!(
        sent <= 192 << 10 && held <= 192 << 10,
        "{sent} bytes sent, {held} held"
    );
}

#[tokio::test]
async fn under_pressure_an_admitted_upload_reserves_no_receive_credit() {
    let h3 = H3::start(&[]).await;
    let budget = h3.server.budget.clone();
    let connection = h3.connect(transport(None)).await;
    let id = h3.server.upload_id().await;
    let pressure = budget.lease(budget.usage().limit / 4 * 3).unwrap();
    let before = settled(&budget).await;
    let (mut upload, mut answer) = connection.open("POST", &format!("/upload?id={id}")).await;
    upload
        .send_data(bytes::Bytes::from_static(b"held back"))
        .await
        .unwrap();
    let held = settled(&budget).await.saturating_sub(before);
    assert!(
        held < CONNECTION_CREDIT / 4,
        "{held} bytes while it reads under pressure"
    );
    upload.finish().await.unwrap();
    assert_eq!(answer.response().await.unwrap().status(), 200);
    assert_eq!(read(&mut answer).await.unwrap(), br#"{"bytes":9}"#);
    drop(pressure);
}

fn exhaust(budget: &Budget, spare: usize) -> graphite_meter_server::limits::Lease {
    for _ in 0..1000 {
        let left = budget.usage().limit - budget.usage().used;
        if let Some(lease) = budget.lease(left.saturating_sub(spare)) {
            return lease;
        }
    }
    panic!("the budget never held still for a lease");
}

#[tokio::test]
async fn an_exhausted_budget_slows_running_connections_and_closes_one_it_cannot_admit() {
    let h3 = H3::start(&[]).await;
    let budget = h3.server.budget.clone();
    let running = h3.connect(transport(None)).await;
    let mut download = running.send("GET", "/download?bytes=4194304", b"").await.1;
    let exhausted = exhaust(&budget, 0);
    let bytes = tokio::time::timeout(Duration::from_secs(10), read(&mut download)).await;
    assert_eq!(
        bytes.unwrap().unwrap().len(),
        4 << 20,
        "refused charges leave the transfer its floors"
    );
    drop(exhausted);

    settled(&budget).await;
    let short = exhaust(&budget, 128 << 10);
    let closed = match h3.connect_from("127.0.0.2", transport(None)).await {
        Ok(refused) => refused.quic.closed().await,
        Err(error) => error,
    };
    let noq::ConnectionError::ConnectionClosed(close) = closed else {
        panic!("{closed:?}")
    };
    // The first refused charge decides the code: the handshake's CRYPTO data or the connection's floor.
    let codes = [
        noq::TransportErrorCode::INTERNAL_ERROR,
        noq::TransportErrorCode::CRYPTO_BUFFER_EXCEEDED,
    ];
    assert!(codes.contains(&close.error_code), "{close:?}");
    assert_eq!(&close.reason[..], b"buffer allocation limit");
    assert_eq!(running.quic.close_reason(), None);
    drop(short);
    assert_eq!(
        running.json("GET", "/probe").await["load"]["active"],
        0,
        "the running connection serves on"
    );
}

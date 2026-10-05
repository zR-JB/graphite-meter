//! WebTransport sessions: the ping bus, stream and datagram downloads, endings with their close codes, and the
//! connections that carried only sessions.

use super::{
    http3::{Connection, H3, pass, transport},
    *,
};
use graphite_meter_http3::{Code, webtransport::Session};
use graphite_meter_proto::bus::Pong;
use http::{Request, Response};
use tokio::time::Instant;

impl Connection {
    /// The session `path` opens, or the answer refusing it.
    pub(super) async fn open_session(&self, path: &str) -> Result<Session, Response<()>> {
        let request = Request::get(format!("https://localhost{path}")).body(()).unwrap();
        let opened = Session::connect(&self.requests, request).await.unwrap();
        opened.map(|(session, _)| session)
    }

    pub(super) async fn session(&self, path: &str) -> Session {
        self.open_session(path).await.unwrap()
    }
}

/// Whether the session is still open after a moment of real time.
pub(super) async fn open(session: &Session) -> bool {
    tokio::time::timeout(Duration::from_millis(50), session.closed())
        .await
        .is_err()
}

pub(super) fn ending(code: u32, reason: &str) -> (u32, String) {
    (code, reason.into())
}

#[tokio::test]
async fn the_ping_bus_answers_each_datagram_and_opens_no_streams() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(None)).await;
    let session = connection.session("/wt/ping").await;
    h3.server.until_active(1).await;
    for message in [&b"PING,"[..], b"PING,7"] {
        session.send_datagram(message).unwrap();
    }
    let pong = Pong::decode(&session.read_datagram().await.unwrap()).unwrap();
    assert_eq!(pong.id, 7, "a malformed message gets no reply");
    let stream = tokio::time::timeout(Duration::from_millis(100), session.accept_uni()).await;
    assert!(stream.is_err(), "the bus opens no streams");
    session.close(0, "").await;
    h3.server.until_active(0).await;
}

#[tokio::test]
async fn a_download_replaces_each_finished_stream_while_the_session_lives() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(None)).await;
    let session = connection.session("/wt/download?bytes=100000&streams=2").await;
    for _ in 0..3 {
        let mut stream = session.accept_uni().await.unwrap();
        let mut received = 0;
        while let Some(chunk) = stream.read_chunk().await.unwrap() {
            received += chunk.len();
        }
        assert_eq!(received, 100_000);
    }
    h3.server.until_active(1).await;
    session.close(0, "").await;
    h3.server.until_active(0).await;
}

#[tokio::test]
async fn a_session_serves_a_peer_granting_minimal_credit() {
    let h3 = H3::start(&[]).await;
    let mut minimal = transport(Some(16));
    minimal.receive_window(64_u32.into());
    let connection = h3.connect(minimal).await;
    let session = connection.session("/wt/download?bytes=20000").await;
    let mut stream = session.accept_uni().await.unwrap();
    let mut received = 0;
    while let Some(chunk) = stream.read_chunk().await.unwrap() {
        received += chunk.len();
    }
    assert_eq!(received, 20_000);
}

#[tokio::test]
async fn a_datagram_download_floods_its_bytes_a_datagram_at_a_time() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(None)).await;
    let session = connection.session("/wt/download?bytes=2500&datagrams").await;
    let mut sizes = Vec::new();
    for _ in 0..50 {
        sizes.push(session.read_datagram().await.unwrap().len());
    }
    assert!(sizes.iter().all(|size| [1000, 500].contains(size)), "{sizes:?}");
    assert!(sizes.contains(&500) && sizes.contains(&1000), "{sizes:?}");
}

#[tokio::test]
async fn refused_connects_are_answered() {
    let h3 = H3::start(&[("GM_MAX_SESSIONS_PER_CLIENT", "1")]).await;
    let (first, second) = (h3.connect(transport(None)).await, h3.connect(transport(None)).await);
    let _session = first.session("/wt/download?bytes=0").await;
    for (path, status) in [("/wt/download?bytes=0", 429), ("/probe", 405), ("/nope", 404)] {
        let refused = second.open_session(path).await.err().expect("a refusal");
        assert_eq!(refused.status(), status, "{path}");
    }
}

#[tokio::test]
async fn an_establish_only_download_closes_after_five_seconds_and_its_connection_a_second_later() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(None)).await;
    let session = connection.session("/wt/download?bytes=0").await;
    pass(Duration::from_millis(4900)).await;
    assert!(open(&session).await);
    pass(Duration::from_millis(100)).await;
    assert_eq!(session.closed().await, Ok(ending(0, "")), "the server finished it");
    session.close(0, "").await;
    assert_eq!(connection.closed_within(Duration::from_millis(50)).await, None, "the CLOSE goes first");
    pass(Duration::from_secs(1)).await;
    assert_eq!(connection.closed_within(Duration::from_secs(2)).await, Some(Code::H3_NO_ERROR));
}

#[tokio::test]
async fn a_connection_that_carried_only_sessions_closes_when_the_peer_ends_its_last() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(None)).await;
    connection.session("/wt/ping").await.close(0, "").await;
    assert_eq!(connection.closed_within(Duration::from_secs(1)).await, Some(Code::H3_NO_ERROR));
    let serving = h3.connect(transport(None)).await;
    assert_eq!(serving.send("GET", "/probe", b"").await.0.status(), 200);
    serving.session("/wt/ping").await.close(0, "").await;
    assert_eq!(serving.closed_within(Duration::from_millis(200)).await, None, "it served a request too");
}

#[tokio::test]
async fn sessions_without_peer_traffic_end_idle_after_thirty_seconds() {
    let h3 = H3::start(&[]).await;
    let (bus, flooded) = (h3.connect(transport(None)).await, h3.connect(transport(None)).await);
    let bus = bus.session("/wt/ping").await;
    let flooded = flooded.session("/wt/download?bytes=1000&datagrams").await;
    pass(Duration::from_secs(25)).await;
    bus.send_datagram(b"PING,1").unwrap();
    bus.read_datagram().await.expect("its PONG");
    pass(Duration::from_secs(5)).await;
    assert!(open(&bus).await, "a datagram is the peer's movement");
    assert_eq!(flooded.closed().await, Ok(ending(1, "idle")), "a flood is the server's own traffic");
    pass(Duration::from_secs(25)).await;
    assert_eq!(bus.closed().await, Ok(ending(1, "idle")));
}

#[tokio::test]
async fn the_bus_ends_at_the_operation_lifetime_and_transfers_at_the_session_lifetime() {
    let env = [("GM_MAX_OPERATION_DURATION", "1s"), ("GM_MAX_SESSION_DURATION", "2s")];
    let h3 = H3::start(&env).await;
    let (bus, transfer) = (h3.connect(transport(None)).await, h3.connect(transport(None)).await);
    let started = Instant::now();
    let (bus, transfer) = (bus.session("/wt/ping").await, transfer.session("/wt/download").await);
    assert_eq!(bus.closed().await, Ok(ending(2, "lifetime")));
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(2), "{elapsed:?}");
    assert_eq!(transfer.closed().await, Ok(ending(2, "lifetime")));
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(3), "{elapsed:?}");
}

#[tokio::test]
async fn shutdown_ends_sessions_and_closes_their_connection_after_the_close() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(None)).await;
    let session = connection.session("/wt/ping").await;
    h3.server.until_active(1).await;
    let stopped = h3.server.stop();
    assert_eq!(session.closed().await, Ok(ending(4, "shutdown")));
    session.close(0, "").await;
    assert_eq!(connection.closed_within(Duration::from_millis(50)).await, None);
    pass(Duration::from_secs(1)).await;
    assert_eq!(connection.closed_within(Duration::from_secs(2)).await, Some(Code::H3_NO_ERROR));
    stopped.await.unwrap().unwrap();
}

/// A QUIC varint below 2^14.
fn varint(value: usize) -> Vec<u8> {
    match value {
        0..64 => vec![value as u8],
        _ => vec![0x40 | (value >> 8) as u8, value as u8],
    }
}

fn frame(kind: usize, payload: &[u8]) -> Vec<u8> {
    [varint(kind), varint(payload.len()), payload.to_vec()].concat()
}

/// A HEADERS frame of literal fields, as a static-only QPACK encoder writes them.
fn headers(fields: &[(&str, &str)]) -> Vec<u8> {
    fn integer(value: usize, bits: u32, pattern: u8, output: &mut Vec<u8>) {
        let max = (1 << bits) - 1;
        if value < max {
            return output.push(pattern | value as u8);
        }
        output.push(pattern | max as u8);
        let mut rest = value - max;
        while rest >= 0x80 {
            output.push(0x80 | rest as u8);
            rest >>= 7;
        }
        output.push(rest as u8);
    }
    let mut section = vec![0, 0];
    for (name, value) in fields {
        integer(name.len(), 3, 0x20, &mut section);
        section.extend_from_slice(name.as_bytes());
        integer(value.len(), 7, 0x00, &mut section);
        section.extend_from_slice(value.as_bytes());
    }
    frame(0x01, &section)
}

#[tokio::test]
async fn a_session_admitted_before_the_shutdown_but_accepted_after_it_still_gets_its_close() {
    let h3 = H3::start(&[]).await;
    let endpoint = noq::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let connecting = endpoint.connect_with(h3.client(transport(None)), h3.server.quic.unwrap(), "localhost");
    let quic = connecting.unwrap().await.unwrap();
    // Without the client's SETTINGS the server cannot accept the session yet.
    let (mut connect, mut answer) = quic.open_bi().await.unwrap();
    let fields = [
        (":method", "CONNECT"),
        (":protocol", "webtransport"),
        (":scheme", "https"),
        (":authority", "localhost"),
        (":path", "/wt/ping"),
    ];
    connect.write_all(&headers(&fields)).await.unwrap();
    h3.server.until_active(1).await;
    let stopped = h3.server.stop();
    let mut control = quic.open_uni().await.unwrap();
    // WebTransport draft 02 and HTTP datagrams.
    let settings = [0xab, 0x60, 0x37, 0x42, 0x01, 0x33, 0x01];
    control
        .write_all(&[&[0x00][..], &frame(0x04, &settings)].concat())
        .await
        .unwrap();
    let answered = answer.read_to_end(4096).await.unwrap();
    let close = frame(0x00, &frame(0x2843, &[&4_u32.to_be_bytes()[..], b"shutdown"].concat()));
    assert_eq!(answered[0], 0x01, "the 200 head first");
    assert!(answered.ends_with(&close), "then CLOSE 4 and FIN");
    connect.finish().unwrap();
    pass(Duration::from_secs(1)).await;
    let closed = tokio::time::timeout(Duration::from_secs(2), quic.closed())
        .await
        .unwrap();
    assert!(
        matches!(&closed, noq::ConnectionError::ApplicationClosed(close) if close.error_code == Code::H3_NO_ERROR.into()),
        "{closed:?}"
    );
    stopped.await.unwrap().unwrap();
}

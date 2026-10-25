//! WebTransport sessions: datagram downloads, minimal credit, the idle ending and the connections they leave.

use super::{
    http3::{Connection, H3, pass, transport},
    *,
};
use graphite_meter_http3::webtransport::{Session, SessionClose};
use http::{Request, Response};

impl Connection {
    pub(super) async fn open_session(&self, path: &str) -> Result<Session, Response<()>> {
        let request = Request::get(format!("https://localhost{path}"))
            .body(())
            .unwrap();
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

pub(super) fn ending(code: u32, reason: &str) -> SessionClose {
    SessionClose {
        code,
        reason: reason.into(),
    }
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
    let session = connection
        .session("/wt/download?bytes=2500&datagrams")
        .await;
    let mut sizes = Vec::new();
    for _ in 0..50 {
        sizes.push(session.read_datagram().await.unwrap().len());
    }
    assert!(
        sizes.iter().all(|size| [1000, 500].contains(size)),
        "{sizes:?}"
    );
    assert!(sizes.contains(&500) && sizes.contains(&1000), "{sizes:?}");
}

#[tokio::test]
async fn connections_idle_open_after_their_sessions_and_refused_connects_hold_no_share() {
    let h3 = H3::start(&[]).await;
    let mut idle = Vec::new();
    for _ in 0..12 {
        let connection = h3.connect(transport(None)).await;
        let refused = connection.open_session("/wt/missing").await.err();
        assert_eq!(
            refused.map(|answer| answer.status()),
            Some(http::StatusCode::NOT_FOUND)
        );
        idle.push(connection);
    }
    let ended = h3.connect(transport(None)).await;
    ended.session("/wt/ping").await.close(0, "").await;
    idle.push(ended);
    pass(Duration::from_secs(45)).await;
    for connection in &idle {
        assert_eq!(
            connection.closed_within(Duration::from_millis(10)).await,
            None,
            "an idle connection gets no close"
        );
        assert!(connection.requests.going_away(), "but is retired");
    }
    let fresh = h3.connect(transport(None)).await;
    let bus = fresh.session("/wt/ping").await;
    bus.send_datagram(b"PING,1").unwrap();
    bus.read_datagram().await.expect("its PONG");
}

#[tokio::test]
async fn sessions_without_peer_traffic_end_idle_after_thirty_seconds() {
    let h3 = H3::start(&[]).await;
    let connections = [
        h3.connect(transport(None)).await,
        h3.connect(transport(None)).await,
    ];
    let bus = connections[0].session("/wt/ping").await;
    let flooded = connections[1]
        .session("/wt/download?bytes=1000&datagrams")
        .await;
    pass(Duration::from_secs(25)).await;
    assert!(
        connections.iter().all(|c| !c.requests.going_away()),
        "an open session keeps its connection from being retired"
    );
    bus.send_datagram(b"PING,1").unwrap();
    bus.read_datagram().await.expect("its PONG");
    pass(Duration::from_secs(5)).await;
    assert!(open(&bus).await, "a datagram is the peer's movement");
    assert_eq!(
        flooded.closed().await,
        Ok(ending(1, "idle")),
        "a flood is the server's own traffic"
    );
    pass(Duration::from_secs(25)).await;
    assert_eq!(bus.closed().await, Ok(ending(1, "idle")));
}

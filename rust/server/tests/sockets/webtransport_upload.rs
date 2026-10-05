//! WebTransport uploads: client streams and datagrams into one aggregate beside HTTP, the progress feed on a server
//! stream, refusals, the stream ceiling, receive credit and the idle ending.

use super::{
    http3::{Connection, H3, pass, transport},
    quic::settled,
    webtransport::{ending, open},
    *,
};
use graphite_meter_http3::{
    Error, WtCode,
    webtransport::{RecvStream, SendStream, Session},
};
use graphite_meter_proto::upload::Record;
use graphite_meter_server::limits::CONNECTION_CREDIT;

/// A progress feed's records as they arrive.
pub(super) struct Feed {
    stream: RecvStream,
    buffered: Vec<u8>,
}

impl Feed {
    /// The next stream the server opens in `session`.
    pub(super) async fn of(session: &Session) -> Self {
        Self {
            stream: session.accept_uni().await.unwrap(),
            buffered: Vec::new(),
        }
    }

    /// The next record, past heartbeats; `None` at the stream's end.
    pub(super) async fn next(&mut self) -> Option<Record> {
        loop {
            if let Some(end) = self.buffered.iter().position(|&byte| byte == b'\n') {
                let line: Vec<u8> = self.buffered.drain(..=end).collect();
                if end > 0 {
                    return Some(Record::decode(&line[..end]).unwrap());
                }
                continue;
            }
            let read = tokio::time::timeout(Duration::from_secs(5), self.stream.read_chunk());
            let chunk = read.await.expect("a record or heartbeat within 5 s").unwrap()?;
            self.buffered.extend_from_slice(&chunk);
        }
    }

    /// The next record past `progress` ones.
    async fn after_progress(&mut self) -> Option<Record> {
        loop {
            match self.next().await {
                Some(Record::Progress(_)) => {}
                other => return other,
            }
        }
    }
}

pub(super) fn refused(record: Option<Record>) -> String {
    match record {
        Some(Record::Error { code, .. }) => code,
        other => panic!("an error record, not {other:?}"),
    }
}

/// A client stream that sent `bytes` bytes.
async fn lane(session: &Session, bytes: usize) -> SendStream {
    let mut stream = session.open_uni().await.unwrap();
    stream.write_all(&vec![1; bytes]).await.unwrap();
    stream
}

/// Waits until the upload's checkpoint counts `bytes`.
async fn until_bytes(control: &Connection, id: &str, bytes: u64) {
    let reached = async {
        while control.json("POST", &format!("/upload/checkpoint?id={id}")).await["bytes"] != bytes {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), reached)
        .await
        .unwrap_or_else(|_| panic!("{bytes} bytes counted"));
}

#[tokio::test]
async fn client_streams_count_beside_http_and_the_feed_completes_after_finalization() {
    let h3 = H3::start(&[]).await;
    let idle = settled(&h3.server.budget).await;
    let control = h3.connect(transport(None)).await;
    let id = control.upload_id().await;
    let connection = h3.connect(transport(None)).await;
    let session = connection.session(&format!("/wt/upload?id={id}")).await;
    let mut feed = Feed::of(&session).await;
    assert_eq!(feed.next().await, Some(Record::Ready));
    for bytes in [100_000, 200_000] {
        lane(&session, bytes).await.finish().unwrap();
    }
    let mut answer = control.send("POST", &format!("/upload?id={id}"), &[7; 500]).await.1;
    assert_eq!(super::http3::read(&mut answer).await.unwrap(), br#"{"bytes":500}"#);
    until_bytes(&control, &id, 300_500).await;
    let funded = settled(&h3.server.budget).await - idle;
    assert!(funded >= CONNECTION_CREDIT, "{funded} bytes once the upload attached");
    let (finalized, _) = control.send("DELETE", &format!("/upload/progress?id={id}"), b"").await;
    assert_eq!(finalized.status(), 204);
    match feed.after_progress().await {
        Some(Record::Complete(counters)) => assert_eq!(counters.bytes(), 300_500),
        other => panic!("a complete record, not {other:?}"),
    }
    assert_eq!(feed.next().await, None, "the feed's stream finishes");
    let _late = lane(&session, 1).await;
    assert_eq!(refused(Feed::of(&session).await.next().await), "invalid", "a lane after finalization");
    assert!(open(&session).await);
}

#[tokio::test]
async fn datagrams_count_only_when_asked_and_their_lane_leaves_once_finalized() {
    let h3 = H3::start(&[]).await;
    let control = h3.connect(transport(None)).await;
    let (id, unasked) = (control.upload_id().await, control.upload_id().await);
    let (asking, ignoring) = (h3.connect(transport(None)).await, h3.connect(transport(None)).await);
    let session = asking.session(&format!("/wt/upload?id={id}&datagrams")).await;
    let mut feed = Feed::of(&session).await;
    assert_eq!(feed.next().await, Some(Record::Ready));
    for _ in 0..3 {
        session.send_datagram(&[1; 100]).unwrap();
    }
    until_bytes(&control, &id, 300).await;
    let (finalized, _) = control.send("DELETE", &format!("/upload/progress?id={id}"), b"").await;
    assert_eq!(finalized.status(), 204);
    match feed.after_progress().await {
        Some(Record::Complete(counters)) => assert_eq!(counters.bytes(), 300),
        other => panic!("a complete record, not {other:?}"),
    }

    let session = ignoring.session(&format!("/wt/upload?id={unasked}&datagrams=0")).await;
    assert_eq!(Feed::of(&session).await.next().await, Some(Record::Ready));
    for _ in 0..3 {
        session.send_datagram(&[1; 100]).unwrap();
    }
    lane(&session, 10).await.finish().unwrap();
    until_bytes(&control, &unasked, 10).await;
}

#[tokio::test]
async fn a_seventeenth_concurrent_stream_is_stopped_with_code_zero() {
    let h3 = H3::start(&[]).await;
    let control = h3.connect(transport(None)).await;
    let id = control.upload_id().await;
    let connection = h3.connect(transport(None)).await;
    let session = connection.session(&format!("/wt/upload?id={id}")).await;
    let mut lanes = Vec::new();
    for _ in 0..16 {
        lanes.push(lane(&session, 10).await);
    }
    until_bytes(&control, &id, 160).await;
    let mut extra = lane(&session, 10).await;
    let stopped = async {
        loop {
            if let Err(error) = extra.write_all(b"x").await {
                return error;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    let stopped = tokio::time::timeout(Duration::from_secs(2), stopped).await.unwrap();
    assert_eq!(stopped, Error::Stopped(WtCode(0).to_http()));
    let counted = control.json("POST", &format!("/upload/checkpoint?id={id}")).await;
    assert_eq!(counted["bytes"], 160, "the stopped stream counts nothing");
}

#[tokio::test]
async fn an_upload_refused_at_connect_sends_its_error_and_closes_after_two_seconds() {
    let h3 = H3::start(&[]).await;
    let idle = settled(&h3.server.budget).await;
    let connection = h3.connect(transport(None)).await;
    let session = connection.session("/wt/upload?id=forged").await;
    let mut feed = Feed::of(&session).await;
    assert_eq!(refused(feed.next().await), "invalid");
    assert_eq!(feed.next().await, None);
    pass(Duration::from_millis(1900)).await;
    assert!(open(&session).await);
    pass(Duration::from_millis(100)).await;
    assert_eq!(session.closed().await, Ok(ending(0, "")));
    let unfunded = settled(&h3.server.budget).await - idle;
    assert!(unfunded < CONNECTION_CREDIT / 4, "{unfunded} bytes for a refused upload");
}

#[tokio::test]
async fn an_upload_session_ends_idle_with_its_bytes_though_unasked_datagrams_arrive() {
    let h3 = H3::start(&[]).await;
    let control = h3.connect(transport(None)).await;
    let id = control.upload_id().await;
    let connection = h3.connect(transport(None)).await;
    let session = connection.session(&format!("/wt/upload?id={id}&datagrams=0")).await;
    let _stalled = lane(&session, 7).await;
    until_bytes(&control, &id, 7).await;
    pass(Duration::from_secs(25)).await;
    session.send_datagram(b"not movement").unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    pass(Duration::from_secs(5)).await;
    assert_eq!(session.closed().await, Ok(ending(1, "idle")));
    // The first control connection idled out without a request.
    let control = h3.connect(transport(None)).await;
    until_bytes(&control, &id, 7).await;
}

//! WebTransport uploads: client streams and datagrams into one aggregate beside HTTP, the progress feed on a server
//! stream and receive credit.

use super::{
    http3::{Connection, H3, transport},
    quic::settled,
    webtransport::open,
    *,
};
use graphite_meter_http3::webtransport::{RecvStream, SendStream, Session};
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

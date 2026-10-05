//! `/wt/upload` (`api/upload.md`): up to 16 client streams, and datagrams when asked, into one upload's aggregate,
//! with its progress feed on one server stream.

use super::drain;
use crate::{
    engine::{ProgressFeed, UploadSink, Uploads},
    lane::Lane,
    peer::ClientKeys,
    transport::body::FUNDING_RETRY,
};
use bytes::Bytes;
use futures_util::{StreamExt, future::OptionFuture, stream::FuturesUnordered};
use graphite_meter_http3::webtransport::{RecvStream, Session};
use graphite_meter_proto::{lane::IDLE_BOUND, refusal::UploadRefusal, upload::Record};
use std::{fmt, future, mem, pin::pin, time::Duration};
use tokio::time::{sleep, timeout};

/// Client streams read at once; more are stopped.
const MAX_STREAMS: usize = 16;
/// A client stream's chunks taken in one read, recorded at once.
const READ_BATCH: usize = 32;
/// A session refused at connect closes this long after its `error` record.
const REFUSAL_LINGER: Duration = Duration::from_secs(2);

/// An upload session's aggregate and how it is reached.
pub struct Upload {
    pub uploads: Uploads,
    pub id: String,
    pub owner: Option<ClientKeys>,
    /// Whether received datagrams count as upload bytes.
    pub datagrams: bool,
}

impl fmt::Debug for Upload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { id, datagrams, .. } = self;
        formatter
            .debug_struct("Upload")
            .field("id", id)
            .field("datagrams", datagrams)
            .finish()
    }
}

impl Upload {
    /// Serves the session until it ends; a refused upload gets its `error` record and closes after a linger. Once
    /// the feed attached, `fund` is asked until it raises the receive window.
    pub(super) async fn serve(self, session: &Session, lane: &Lane, mut fund: impl FnMut() -> bool) {
        let feed = match self.uploads.subscribe(&self.id, self.owner.as_ref()) {
            Ok(feed) => feed,
            Err(refusal) => {
                let refused = async {
                    refuse(session, refusal).await;
                    sleep(REFUSAL_LINGER).await;
                };
                tokio::select! {
                    () = refused => {}
                    () = drain(session, drop) => {}
                }
                return;
            }
        };
        let funding = async {
            while !fund() {
                sleep(FUNDING_RETRY).await;
            }
        };
        let mut background = pin!(async {
            tokio::join!(funding, progress(session, feed));
            future::pending::<()>().await;
        });
        let mut datagrams = self.datagrams.then(|| self.join(lane).ok()).flatten();
        let mut finalized = pin!(OptionFuture::from(datagrams.as_ref().map(UploadSink::finished)));
        let (mut streams, mut refusals) = (FuturesUnordered::new(), FuturesUnordered::new());
        loop {
            tokio::select! {
                () = &mut background => {}
                // A datagram lane has no FIN: it leaves once the upload is finalized.
                _ = &mut finalized, if datagrams.is_some() => datagrams = None,
                Some(()) = streams.next() => {}
                Some(()) = refusals.next() => {}
                datagram = session.read_datagram() => match (datagram, &mut datagrams) {
                    (Some(datagram), Some(sink)) => sink.record(datagram.len()),
                    (Some(_), None) => {}
                    (None, _) => return,
                },
                stream = session.accept_uni() => {
                    let Some(stream) = stream else { return };
                    // Dropping a stream stops it with code 0.
                    if streams.len() < MAX_STREAMS {
                        match self.join(lane) {
                            Ok(sink) => streams.push(receive(stream, sink)),
                            Err(refusal) if refusals.is_empty() => refusals.push(refuse(session, refusal)),
                            Err(_) => {}
                        }
                    }
                }
            }
        }
    }

    fn join(&self, lane: &Lane) -> Result<UploadSink, UploadRefusal> {
        self.uploads.begin(&self.id, self.owner.as_ref(), lane.clone())
    }
}

/// A client stream's bytes, a batch at a time, until its FIN or `IDLE_BOUND` without any.
async fn receive(mut stream: RecvStream, mut sink: UploadSink) {
    let mut chunks = [const { Bytes::new() }; READ_BATCH];
    while let Ok(Ok(Some(count))) = timeout(IDLE_BOUND, stream.read_chunks(&mut chunks)).await {
        sink.record(chunks[..count].iter_mut().map(|chunk| mem::take(chunk).len()).sum());
    }
}

/// The feed's lines on a server stream, which finishes with the feed.
async fn progress(session: &Session, mut feed: ProgressFeed) {
    let Ok(mut stream) = session.open_uni().await else {
        return;
    };
    while let Some(line) = feed.next().await {
        if stream.write_chunk(line).await.is_err() {
            return;
        }
    }
    let _ = stream.finish();
}

/// `refusal`'s `error` record on a server stream of its own.
async fn refuse(session: &Session, refusal: UploadRefusal) {
    let Ok(mut stream) = session.open_uni().await else {
        return;
    };
    if stream.write_all(Record::from(refusal).line().as_bytes()).await.is_ok() {
        let _ = stream.finish();
    }
}

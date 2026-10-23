//! `/wt/upload` (`api/upload.md`): up to 16 client streams and optional datagrams into one aggregate with a feed.

use super::drain;
use crate::{
    app::query::MAX_LANE_STREAMS,
    engine::{ProgressFeed, UploadSink, Uploads},
    lane::Lane,
    peer::ClientKeys,
    transport::window::FUNDING_RETRY,
};
use bytes::Bytes;
use futures_util::{
    FutureExt, StreamExt,
    future::{Fuse, FusedFuture, OptionFuture},
    stream::FuturesUnordered,
};
use graphite_meter_http3::webtransport::{RecvStream, Session};
use graphite_meter_proto::{lane::IDLE_BOUND, refusal::Refusal, upload::Record};
use std::{mem, pin::pin, time::Duration};
use tokio::time::{sleep, timeout};

const CHUNKS_PER_READ: usize = 32;
const REFUSAL_LINGER: Duration = Duration::from_secs(2);

pub struct Upload {
    pub uploads: Uploads,
    pub id: String,
    pub owner: Option<ClientKeys>,
    pub datagrams: bool,
}

impl Upload {
    /// Serves the session until it ends; refusals get an `error` record and close after a linger; `fund` once fed.
    pub(super) async fn serve(
        self,
        session: &Session,
        lane: &Lane,
        mut fund: impl FnMut() -> bool,
    ) {
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
        let mut background = pin!(async { tokio::join!(funding, progress(session, feed)) }.fuse());
        // The session is one transfer however many streams it reads.
        let transfer = self.uploads.meter().open();
        let join = || {
            self.uploads.begin(
                &self.id,
                self.owner.as_ref(),
                lane.clone(),
                transfer.clone(),
            )
        };
        let mut datagrams = self.datagrams.then(|| join().ok()).flatten();
        let mut finalized = pin!(OptionFuture::from(
            datagrams.as_ref().map(UploadSink::finished)
        ));
        let (mut streams, mut refusal) = (FuturesUnordered::new(), pin!(Fuse::terminated()));
        loop {
            tokio::select! {
                _ = &mut background => {}
                // A datagram lane has no FIN: it leaves once the upload is finalized.
                _ = &mut finalized, if datagrams.is_some() => datagrams = None,
                Some(()) = streams.next() => {}
                () = &mut refusal => {}
                datagram = session.read_datagram() => match (datagram, &mut datagrams) {
                    (Some(datagram), Some(sink)) => sink.record(datagram.len()),
                    (Some(_), None) => {}
                    (None, _) => return,
                },
                stream = session.accept_uni() => {
                    let Some(stream) = stream else { return };
                    // Dropping a stream stops it with code 0.
                    if streams.len() < MAX_LANE_STREAMS {
                        match join() {
                            Ok(sink) => streams.push(receive(stream, sink)),
                            Err(refused) if refusal.is_terminated() => refusal.set(refuse(session, refused).fuse()),
                            Err(_) => {}
                        }
                    }
                }
            }
        }
    }
}

/// A client stream's bytes, a batch at a time, until its FIN or `IDLE_BOUND` without any.
async fn receive(mut stream: RecvStream, mut sink: UploadSink) {
    let mut chunks = [const { Bytes::new() }; CHUNKS_PER_READ];
    while let Ok(Ok(Some(count))) = timeout(IDLE_BOUND, stream.read_chunks(&mut chunks)).await {
        sink.record(
            chunks[..count]
                .iter_mut()
                .map(|chunk| mem::take(chunk).len())
                .sum(),
        );
    }
}

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

async fn refuse(session: &Session, refusal: Refusal) {
    let Ok(mut stream) = session.open_uni().await else {
        return;
    };
    if stream
        .write_all(Record::from(refusal).line().as_bytes())
        .await
        .is_ok()
    {
        let _ = stream.finish();
    }
}

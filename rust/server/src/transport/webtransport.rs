//! WebTransport sessions (`api/wire.md#webtransport-routes`): admitted, served over a lane, closed with its code.

mod upload;

pub use upload::Upload;

use crate::{
    engine::{DownloadSource, reflect},
    lane::Lane,
};
use bytes::Bytes;
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_http3::{RequestStream, webtransport::Session};
use http::HeaderMap;
use std::time::Duration;
use tokio::time::sleep;

/// A CONNECT that opens no session is answered within this long.
pub const ANSWER_BOUND: Duration = Duration::from_secs(10);
/// An establish-only download session closes after this long; its answer is the handshake.
const VERIFY_LINGER: Duration = Duration::from_secs(5);
/// A download datagram carries this much.
const DATAGRAM_BYTES: usize = 1000;
/// A datagram flood yields after this many sends, so sibling sessions run without a handoff per packet.
const DATAGRAM_YIELD_BATCH: usize = 16;
/// A download stream writes at most this much at a time.
const LANE_WRITE_BYTES: usize = 16 << 10;

/// What an admitted session serves.
pub enum Plan {
    /// The latency bus, one message per datagram.
    Ping,
    /// `source`'s bytes on `streams` server streams, or repeated in datagrams.
    Download { source: DownloadSource, streams: usize, datagrams: bool },
    /// Client streams, and datagrams when asked, into an upload, with its progress feed on a server stream.
    Upload(Upload),
}

/// Accepts `stream`'s session, serving `plan` until `lane` ends, the peer closes or the route ends; uploads ask `fund`.
pub async fn serve(stream: RequestStream, headers: HeaderMap, lane: Lane, plan: Plan, fund: impl FnMut() -> bool) {
    let Ok(session) = Session::accept(stream, headers).await else {
        return;
    };
    let ending = {
        let routed = route(&session, &lane, plan, fund);
        tokio::select! {
            biased;
            ending = lane.ended() => ending,
            _ = session.closed() => lane.finish(),
            () = routed => lane.finish(),
        }
    };
    session.close(ending.webtransport_code(), ending.reason()).await;
}

async fn route(session: &Session, lane: &Lane, plan: Plan, fund: impl FnMut() -> bool) {
    match plan {
        Plan::Ping => ping(session, lane).await,
        Plan::Download { source, streams, datagrams } => download(session, lane, source, streams, datagrams).await,
        Plan::Upload(upload) => upload.serve(session, lane, fund).await,
    }
}

/// Hands every datagram to `received` and refuses peer streams until the session ends; unread datagrams queue.
async fn drain(session: &Session, mut received: impl FnMut(Bytes)) {
    loop {
        tokio::select! {
            datagram = session.read_datagram() => match datagram {
                Some(datagram) => received(datagram),
                None => return,
            },
            stream = session.accept_uni() => if stream.is_none() {
                return;
            },
        }
    }
}

/// Every datagram is the peer's movement; each valid PING gets its PONG.
async fn ping(session: &Session, lane: &Lane) {
    drain(session, |datagram| {
        let received = std::time::Instant::now();
        lane.moved();
        if let Some(pong) = reflect(&datagram, received) {
            let _ = session.send_datagram(pong.as_bytes());
        }
    })
    .await;
}

/// Streams or a datagram flood; only the peer's datagrams in the datagram form and drained streams are movement.
async fn download(session: &Session, lane: &Lane, source: DownloadSource, streams: usize, datagrams: bool) {
    let transfer = async {
        if source.remaining() == 0 {
            sleep(VERIFY_LINGER).await;
        } else if datagrams {
            flood(session, &source).await;
        } else {
            let mut lanes: FuturesUnordered<_> =
                (0..streams).map(|_| download_stream(session, &source, lane)).collect();
            while lanes.next().await.is_some() {}
        }
    };
    tokio::select! {
        () = transfer => {}
        () = drain(session, |_| if datagrams { lane.moved() }) => {}
    }
}

/// Writes `source` on a stream, replacing each one the peer drained; a stream the peer took nothing from ends the lane.
async fn download_stream(session: &Session, source: &DownloadSource, lane: &Lane) {
    loop {
        let Ok(mut stream) = session.open_uni().await else {
            return;
        };
        let (mut payload, mut moved) = (source.clone(), false);
        while let Some(chunk) = payload.next(LANE_WRITE_BYTES) {
            if stream.write_chunk(chunk).await.is_err() {
                break;
            }
            moved = true;
            lane.moved();
            if lane.due().is_some() {
                return;
            }
        }
        if !moved {
            return;
        }
        if payload.remaining() == 0 {
            let _ = stream.finish();
        }
        tokio::task::yield_now().await;
    }
}

/// Repeats `source`'s bytes in datagrams until the session ends or refuses one.
async fn flood(session: &Session, source: &DownloadSource) {
    let count = source.remaining();
    let Some(full) = source.peek(DATAGRAM_BYTES) else {
        return;
    };
    let tail = (count % full.len() as u64) as usize;
    let Ok(mut full_datagram) = session.prepare_datagram(&full) else {
        return;
    };
    let tail_datagram = (tail > 0).then(|| session.prepare_datagram(&full[..tail]));
    let Ok(mut tail_datagram) = tail_datagram.transpose() else {
        return;
    };
    let mut since_yield = 0;
    loop {
        let mut remaining = count;
        while remaining > 0 {
            let (datagram, size) = match &mut tail_datagram {
                Some(datagram) if remaining < full.len() as u64 => (datagram, tail),
                _ => (&mut full_datagram, full.len()),
            };
            if datagram.send_wait().await.is_err() {
                return;
            }
            source.sent(size);
            remaining -= size as u64;
            since_yield += 1;
            if since_yield == DATAGRAM_YIELD_BATCH {
                since_yield = 0;
                tokio::task::yield_now().await;
            }
        }
    }
}

//! A session's unidirectional streams: once the session ended, reads and writes end with WT_SESSION_GONE.
use super::{Phase, registry::Unrouted, unless_ended};
use crate::{
    budget::Charge,
    code::{Code, WtCode},
    driver::Shared,
    error::Error,
    frame::{self, Header},
    stream::poll_write_header,
};
use bytes::Bytes;
use std::{
    future::poll_fn,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

/// How long a cancelled stream may take to deliver its association header.
const RESET_DEADLINE: Duration = Duration::from_secs(10);
const LANE_CANCELLED: WtCode = WtCode(0);

/// A peer's stream in a session: noq's chunks as they arrived. Dropping it unread cancels the lane.
pub struct RecvStream {
    stream: noq::RecvStream,
    first: Bytes,
    done: bool,
    session: watch::Receiver<Phase>,
    _charge: Charge,
}

impl RecvStream {
    pub(super) fn new(unrouted: Unrouted, session: watch::Receiver<Phase>) -> Self {
        let Unrouted { stream, first, charge } = unrouted;
        Self { stream, first, done: false, session, _charge: charge }
    }

    /// The next chunk, `None` at the stream's end.
    pub async fn read_chunk(&mut self) -> Result<Option<Bytes>, Error> {
        let mut chunk = [Bytes::new()];
        Ok(self
            .read_chunks(&mut chunk)
            .await?
            .map(|_| std::mem::take(&mut chunk[0])))
    }

    /// Fills `chunks` with the next chunks under one lock of the connection: how many, `None` at the end.
    pub async fn read_chunks(&mut self, chunks: &mut [Bytes]) -> Result<Option<usize>, Error> {
        let (first, stream) = (&mut self.first, &mut self.stream);
        let read = async {
            if first.is_empty() {
                return stream.read_many_chunks(chunks).await;
            }
            chunks[0] = std::mem::take(first);
            Ok(Some(1))
        };
        let Ok(read) = unless_ended(&self.session, read).await else {
            self.stop(Code::WT_SESSION_GONE);
            return Err(Error::Refused);
        };
        self.done |= !matches!(read, Ok(Some(_)));
        Ok(read?)
    }

    /// Stops the stream with `code`, or once its session ended with WT_SESSION_GONE.
    pub fn stop(&mut self, code: Code) {
        if !std::mem::replace(&mut self.done, true) {
            let code = if self.session.borrow().is_ended() { Code::WT_SESSION_GONE } else { code };
            let _ = self.stream.stop(code.into());
        }
    }
}

impl Drop for RecvStream {
    fn drop(&mut self) {
        self.stop(LANE_CANCELLED.to_http());
    }
}

/// A stream this side opened; dropped unfinished it resets past its header, which RESET_STREAM_AT peers still get.
pub struct SendStream {
    lane: Option<Lane>,
    session: watch::Receiver<Phase>,
    shared: Arc<Shared>,
}

impl SendStream {
    pub(super) async fn open(shared: &Arc<Shared>, id: u64, session: watch::Receiver<Phase>) -> Result<Self, Error> {
        let stream = shared.quic.open_uni().await?;
        let lane = Lane {
            stream,
            header: Header::new(frame::WEBTRANSPORT_STREAM, id),
            code: LANE_CANCELLED.to_http(),
        };
        let mut opened = Self { lane: Some(lane), session, shared: shared.clone() };
        let lane = opened.lane.as_mut().expect("open stream");
        poll_fn(|cx| poll_write_header(&mut lane.header, &mut lane.stream, cx)).await?;
        Ok(opened)
    }

    /// The stream, refused once its session ended.
    fn stream(&mut self) -> Result<(&mut noq::SendStream, &watch::Receiver<Phase>), Error> {
        if self.session.borrow().is_ended() {
            return Err(Error::Refused);
        }
        Ok((&mut self.lane.as_mut().expect("open stream").stream, &self.session))
    }

    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let (stream, session) = self.stream()?;
        Ok(unless_ended(session, stream.write_all(bytes)).await??)
    }

    /// noq keeps `chunk` uncopied, charged by length, so it must not pin a larger buffer.
    pub async fn write_chunk(&mut self, chunk: Bytes) -> Result<(), Error> {
        let mut chunks = [chunk];
        let mut unwritten = &mut chunks[..];
        while !unwritten.is_empty() {
            let (stream, session) = self.stream()?;
            unless_ended(session, stream.write_leased_chunks(&mut unwritten)).await??;
        }
        Ok(())
    }

    /// Refused once the session ended, when the stream is reset instead.
    pub fn finish(mut self) -> Result<(), Error> {
        self.stream()?;
        let mut lane = self.lane.take().expect("open stream");
        lane.stream
            .finish()
            .map_err(|_| Error::Stopped(Code::H3_REQUEST_CANCELLED))
    }

    pub fn reset(mut self, code: WtCode) {
        self.lane.as_mut().expect("open stream").code = code.to_http();
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        let Some(mut lane) = self.lane.take() else { return };
        if self.session.borrow().is_ended() {
            lane.code = Code::WT_SESSION_GONE;
        }
        if lane.header.is_written() {
            lane.cancel();
        } else {
            let deadline = Instant::now() + RESET_DEADLINE;
            self.shared
                .sessions()
                .defer(PendingReset { lane, deadline, charge: None });
            self.shared.wake();
        }
    }
}

struct Lane {
    stream: noq::SendStream,
    header: Header,
    code: Code,
}

impl Lane {
    /// Resets after the association header, with plain RESET_STREAM for peers without reliable reset.
    fn cancel(&mut self) {
        let reliable = noq::VarInt::from_u32(u32::from(self.header.len()));
        if let Err(noq::ResetStreamAtError::Unsupported) = self.stream.reset_at(reliable, self.code.into()) {
            let _ = self.stream.reset(self.code.into());
        }
    }
}

/// A cancelled stream whose association header is still being written; the driver completes it.
pub(crate) struct PendingReset {
    lane: Lane,
    pub(crate) deadline: Instant,
    pub(crate) charge: Option<Charge>,
}

impl PendingReset {
    /// Resets once the header is out; `true` when the stream is done with.
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, now: Instant) -> bool {
        if now >= self.deadline {
            self.abandon();
            return true;
        }
        match poll_write_header(&mut self.lane.header, &mut self.lane.stream, cx) {
            Poll::Pending => false,
            Poll::Ready(written) => {
                if written.is_ok() {
                    self.lane.cancel();
                }
                true
            }
        }
    }

    /// Resets without the association: never a FIN.
    pub(crate) fn abandon(&mut self) {
        let _ = self.lane.stream.reset(self.lane.code.into());
    }
}

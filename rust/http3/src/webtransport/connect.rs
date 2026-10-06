//! What the driver runs for sessions: CONNECT streams past their head, so each closes properly, and cancelled headers.
use super::{Phase, stream::PendingReset};
use crate::{budget::Charge, capsule, code::Code, driver::Shared, error::Error, frame, stream::RequestStream};
use bytes::Bytes;
use std::{
    task::{Context, Poll},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

/// How long a closing session waits for the peer's FIN, and a sessions-only connection for the CLOSE.
const CLOSE_WAIT: Duration = Duration::from_secs(1);
/// Control data a peer may send on its CONNECT stream.
const MAX_CONNECT_BYTES: u64 = 1024 * 1024;
const MAX_CONNECT_CHUNKS: u64 = 1024;
const MAX_DEFERRED_RESETS: usize = 64;

#[derive(Default)]
pub(crate) struct Sessions {
    connects: Vec<Connect>,
    resets: Vec<PendingReset>,
}

impl Sessions {
    /// Runs every CONNECT stream and pending reset; `Ready` once one is done with.
    pub(crate) fn poll(&mut self, cx: &mut Context<'_>, shared: &Shared) -> Poll<()> {
        let (connects, resets) = shared.sessions().take();
        self.connects.extend(connects);
        for reset in resets {
            self.defer(reset, shared);
        }
        let now = Instant::now();
        let before = self.connects.len() + self.resets.len();
        self.connects.retain_mut(|connect| !connect.poll(cx, now, shared));
        self.resets.retain_mut(|reset| !reset.poll(cx, now));
        if self.connects.len() + self.resets.len() < before {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    /// Queues a reset within the bound and the budget; past either it resets without its header.
    fn defer(&mut self, mut reset: PendingReset, shared: &Shared) {
        reset.charge = Charge::new(&shared.budget, size_of::<PendingReset>());
        if self.resets.len() < MAX_DEFERRED_RESETS && reset.charge.is_some() {
            self.resets.push(reset);
        } else {
            reset.abandon();
        }
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        let closing = self.connects.iter().filter_map(|connect| connect.deadline);
        closing.chain(self.resets.iter().map(|reset| reset.deadline)).min()
    }

    /// The connection ended with `error`: every session ends with it, and no session follows.
    pub(crate) fn end(&mut self, shared: &Shared, error: &Error) {
        let (connects, resets) = shared.sessions().end();
        for connect in self.connects.drain(..).chain(connects) {
            connect.end(Err(error.clone()));
        }
        self.resets
            .drain(..)
            .chain(resets)
            .for_each(|mut reset| reset.abandon());
    }
}

/// A session's CONNECT stream: the driver reads the peer's capsules and runs the close.
pub(crate) struct Connect {
    pub(super) id: u64,
    stream: RequestStream,
    capsules: capsule::Reader,
    input: Bytes,
    /// Control data the peer sent: bytes and chunks.
    received: (u64, u64),
    pub(super) phase: watch::Sender<Phase>,
    /// Once the session ended: when the wait for the peer's FIN gives up.
    deadline: Option<Instant>,
    peer_finished: bool,
    /// The peer's CLOSE arrived, so only its FIN may follow.
    peer_closed: bool,
    /// This side ended the session, so a sessions-only connection lingers for its CLOSE.
    closed_here: bool,
    _charge: Charge,
}

impl Connect {
    pub(super) fn new(id: u64, stream: RequestStream, phase: watch::Sender<Phase>, charge: Charge) -> Self {
        Self {
            id,
            stream,
            capsules: capsule::Reader::default(),
            input: Bytes::new(),
            received: (0, 0),
            phase,
            deadline: None,
            peer_finished: false,
            peer_closed: false,
            closed_here: false,
            _charge: charge,
        }
    }

    /// Ends the session: CLOSE unless peer-ended, FIN, the peer's FIN within 1 s, then STOP_SENDING; `true` once done.
    fn poll(&mut self, cx: &mut Context<'_>, now: Instant, shared: &Shared) -> bool {
        let failed = match self.poll_read(cx) {
            Ok(()) if self.deadline.is_none() => match self.stream.send.poll_ready(cx) {
                Poll::Ready(Ok(())) => {
                    self.phase.send_if_modified(|phase| {
                        matches!(phase, Phase::Opening) && {
                            *phase = Phase::Open;
                            true
                        }
                    });
                    None
                }
                Poll::Ready(Err(error)) => Some(error),
                Poll::Pending => None,
            },
            read => read.err(),
        };
        if let Some(error) = failed {
            self.end(Err(error));
            return self.done(now, shared);
        }
        let Some(deadline) = self.deadline.or_else(|| self.close(now, shared)) else {
            return false;
        };
        let finished = self.stream.send.poll_finish(cx).is_ready();
        if !(finished && self.peer_finished) && now < deadline {
            return false;
        }
        // Browsers drop the code of a CLOSE whose STOP_SENDING arrives first.
        self.stream.send.reset(Code::WT_SESSION_GONE);
        self.stream.recv.stop(Code::WT_SESSION_GONE);
        self.done(now, shared)
    }

    /// Starts the close once the session ended or this side asked; returns when the wait for FIN ends.
    fn close(&mut self, now: Instant, shared: &Shared) -> Option<Instant> {
        if !self.phase.borrow().is_ended() {
            let (code, reason) = shared.sessions().requested_close(self.id)?;
            // An unwritten 200 head goes first.
            self.stream
                .send
                .queue_after(frame::DATA, &capsule::close(code, &reason));
            self.end(Ok((code, reason)));
            self.closed_here = true;
        }
        shared.sessions().unregister(self.id);
        Some(*self.deadline.insert(now + CLOSE_WAIT))
    }

    fn done(&self, now: Instant, shared: &Shared) -> bool {
        let mut sessions = shared.sessions();
        sessions.unregister(self.id);
        if self.closed_here {
            sessions.linger = Some(now + CLOSE_WAIT);
        }
        true
    }

    /// Reads capsules to the peer's FIN; its CLOSE or a bare FIN ends the session; data past CLOSE is H3_MESSAGE_ERROR.
    fn poll_read(&mut self, cx: &mut Context<'_>) -> Result<(), Error> {
        while !self.peer_finished {
            if self.peer_closed && !self.input.is_empty() {
                return Err(self.stream.abort(Code::H3_MESSAGE_ERROR));
            }
            match self.capsules.read(&mut self.input) {
                Err(code) => return Err(self.stream.abort(code)),
                Ok(Some(close)) => {
                    self.peer_closed = true;
                    self.end(Ok(close));
                }
                Ok(None) => match self.stream.recv.poll_data(cx) {
                    Poll::Pending => break,
                    Poll::Ready(Ok(Some(data))) => {
                        self.received = (self.received.0 + data.len() as u64, self.received.1 + 1);
                        if self.received.0 > MAX_CONNECT_BYTES || self.received.1 > MAX_CONNECT_CHUNKS {
                            return Err(self.stream.abort(Code::H3_EXCESSIVE_LOAD));
                        }
                        self.input = data;
                    }
                    Poll::Ready(Ok(None)) if self.capsules.at_boundary() => {
                        self.peer_finished = true;
                        self.end(Ok((0, String::new())));
                    }
                    Poll::Ready(Ok(None)) => return Err(self.stream.abort(Code::H3_MESSAGE_ERROR)),
                    Poll::Ready(Err(error)) => return Err(error),
                },
            }
        }
        Ok(())
    }

    /// The first ending stands.
    pub(crate) fn end(&self, ending: Result<(u32, String), Error>) {
        self.phase.send_if_modified(|phase| {
            !phase.is_ended() && {
                *phase = Phase::Ended(ending);
                true
            }
        });
    }
}

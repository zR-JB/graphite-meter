//! The connection's one session for routing streams and datagrams, peer streams that arrived before it,
//! and what handles leave for the driver to finish.
use super::{
    Phase,
    connect::Connect,
    stream::{PendingReset, RecvStream},
};
use crate::{budget::Charge, code::Code};
use bytes::Bytes;
use std::{collections::VecDeque, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};

const EARLY_WAIT: Duration = Duration::from_secs(5);
const MAX_EARLY: usize = 64;
pub(super) const STREAM_QUEUE: usize = 32;
pub(super) const DATAGRAM_QUEUE: usize = 256;

/// A peer's session stream before it reaches its session.
pub(crate) struct Unrouted {
    pub(crate) stream: noq::RecvStream,
    /// Bytes that followed the stream header in its first chunk.
    pub(crate) first: Bytes,
    pub(crate) charge: Charge,
}

impl Unrouted {
    fn refuse(mut self, code: Code) {
        let _ = self.stream.stop(code.into());
    }
}

#[derive(Default)]
pub(crate) struct Registry {
    active: Option<Active>,
    early: VecDeque<(Instant, u64, Unrouted)>,
    /// Sessions at or below this ID have ended.
    gone_through: Option<u64>,
    /// The close every session gets once the server shuts down.
    shutdown: Option<(u32, String)>,
    /// A sessions-only connection stays open until then, so the CLOSE of a session this side ended
    /// arrives first.
    pub(crate) linger: Option<Instant>,
    /// Whether the connection carried a session, and served anything else.
    carried: bool,
    pub(crate) served: bool,
    /// CONNECT streams and cancelled streams the driver has yet to take.
    handed: (Vec<Connect>, Vec<PendingReset>),
    ended: bool,
}

struct Active {
    id: u64,
    streams: mpsc::Sender<RecvStream>,
    datagrams: mpsc::Sender<Bytes>,
    phase: watch::Receiver<Phase>,
    /// This side's close, from the application or the shutdown; the first one stands.
    close: Option<(u32, String)>,
}

pub(super) type Queues = (mpsc::Receiver<RecvStream>, mpsc::Receiver<Bytes>);

impl Registry {
    pub(crate) fn stream(&mut self, session: u64, stream: Unrouted) {
        match &self.active {
            Some(active) if active.id == session => {
                let stream = RecvStream::new(stream, active.phase.clone());
                match active.streams.try_send(stream) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(mut stream)) => stream.stop(Code::WT_BUFFERED_STREAM_REJECTED),
                    Err(mpsc::error::TrySendError::Closed(mut stream)) => stream.stop(Code::WT_SESSION_GONE),
                }
            }
            _ if self.gone_through.is_some_and(|gone| session <= gone) => stream.refuse(Code::WT_SESSION_GONE),
            _ if self.early.len() < MAX_EARLY => self.early.push_back((Instant::now() + EARLY_WAIT, session, stream)),
            _ => stream.refuse(Code::WT_BUFFERED_STREAM_REJECTED),
        }
    }

    /// Datagrams are unreliable: one for no current session, or over the queue, is dropped.
    pub(crate) fn datagram(&mut self, session: u64, payload: Bytes) {
        if let Some(active) = self.active(session) {
            let _ = active.datagrams.try_send(payload);
        }
    }

    /// Refuses streams that waited too long for their session.
    pub(crate) fn expire(&mut self, now: Instant) {
        while self.early.front().is_some_and(|(deadline, ..)| *deadline <= now) {
            let (_, _, stream) = self.early.pop_front().expect("an early stream");
            stream.refuse(Code::WT_BUFFERED_STREAM_REJECTED);
        }
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.early.front().map(|(deadline, ..)| *deadline)
    }

    /// A connection that carried sessions and served nothing else ends with its last session.
    pub(crate) fn only_sessions(&self) -> bool {
        self.carried && !self.served
    }

    /// Server: every session, current or yet to come, ends with this close.
    pub(crate) fn shutdown(&mut self, code: u32, reason: &str) {
        self.shutdown = Some((code, reason.into()));
        if let Some(active) = &mut self.active {
            active.close.get_or_insert_with(|| (code, reason.into()));
        }
    }

    /// The connection ended: so do the session's streams and datagrams, and no session follows. Returns
    /// what the driver had yet to take.
    pub(super) fn end(&mut self) -> (Vec<Connect>, Vec<PendingReset>) {
        (self.active, self.ended) = (None, true);
        self.early.clear();
        self.take()
    }

    pub(super) fn take(&mut self) -> (Vec<Connect>, Vec<PendingReset>) {
        std::mem::take(&mut self.handed)
    }

    /// Leaves a cancelled stream for the driver, or resets it at once if the connection ended.
    pub(super) fn defer(&mut self, mut reset: PendingReset) {
        if self.ended {
            reset.abandon();
        } else {
            self.handed.1.push(reset);
        }
    }

    /// Whether a session may start: the connection has none and has not ended.
    pub(super) fn vacant(&self) -> bool {
        self.active.is_none() && !self.ended
    }

    /// Makes `connect` the connection's session, which must be vacant, for the driver to run.
    pub(super) fn register(&mut self, connect: Connect) -> Queues {
        let (streams, stream_queue) = mpsc::channel(STREAM_QUEUE);
        let (datagrams, datagram_queue) = mpsc::channel(DATAGRAM_QUEUE);
        let (id, phase, close) = (connect.id, connect.phase.subscribe(), self.shutdown.clone());
        self.active = Some(Active { id, streams, datagrams, phase, close });
        self.carried = true;
        self.handed.0.push(connect);
        for (deadline, session, stream) in std::mem::take(&mut self.early) {
            if session == id {
                self.stream(session, stream);
            } else {
                self.early.push_back((deadline, session, stream));
            }
        }
        (stream_queue, datagram_queue)
    }

    pub(super) fn request_close(&mut self, id: u64, code: u32, reason: &str) {
        if let Some(active) = self.active(id) {
            active.close.get_or_insert_with(|| (code, reason.into()));
        }
    }

    pub(super) fn requested_close(&mut self, id: u64) -> Option<(u32, String)> {
        self.active(id).and_then(|active| active.close.clone())
    }

    /// The session is gone for routing; later streams naming it are refused.
    pub(super) fn unregister(&mut self, id: u64) {
        if self.active(id).is_some() {
            self.active = None;
        }
        self.gone_through = self.gone_through.max(Some(id));
    }

    fn active(&mut self, id: u64) -> Option<&mut Active> {
        self.active.as_mut().filter(|active| active.id == id)
    }
}

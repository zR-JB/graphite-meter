//! When an upload raises its connection's receive window, and the request body that asks as it reads.

use crate::{
    exchange::Watch,
    limits::{Budget, Pressure},
    peer::ClientKeys,
};
use bytes::Bytes;
use http_body::Frame;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::time::Instant;

/// An upload refused a raised receive window asks again after this pause.
pub(crate) const FUNDING_RETRY: Duration = Duration::from_millis(100);

/// Whether an upload reads at its connection's raised receive window: asked on its first read after admission, and
/// a pause after a refusal; never while the budget holds growth back.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Funding {
    /// A refusal holds the next ask until the instant.
    Unfunded(Option<Instant>),
    Funded,
}

impl Funding {
    /// Asks `raise` with the admitted client's keys once that is due.
    pub(crate) fn fund(&mut self, watch: &Watch, budget: &Budget, raise: impl FnOnce(&ClientKeys) -> bool) {
        let Self::Unfunded(retry) = *self else {
            return;
        };
        let Some(keys) = watch.admitted() else {
            return;
        };
        if budget.pressure() == Pressure::HoldBack || retry.is_some_and(|at| Instant::now() < at) {
            return;
        }
        *self = match raise(keys) {
            true => Self::Funded,
            false => Self::Unfunded(Some(Instant::now() + FUNDING_RETRY)),
        };
    }
}

/// A connection's receive window as its request bodies read it.
pub(crate) trait ReceiveWindow: Send + Sync + 'static {
    type Stream: Send + Unpin;
    type Error;

    fn budget(&self) -> &Budget;

    /// Whether an upload of `keys` reads at the raised window, raising it for the first.
    fn raise(&self, stream: &mut Self::Stream, keys: &ClientKeys) -> bool;

    /// An admitted upload began (`true`) or stopped reading on the connection, funded or not.
    fn reading(&self, _stream: &mut Self::Stream, _reading: bool) {}

    fn poll_data(stream: &mut Self::Stream, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, Self::Error>>>;

    fn is_end_stream(_stream: &Self::Stream) -> bool {
        false
    }
}

/// A request body, which raises its window as `Funding` rules once its exchange is admitted.
pub(crate) struct Incoming<W: ReceiveWindow> {
    stream: W::Stream,
    watch: Watch,
    window: Arc<W>,
    funding: Funding,
    reading: bool,
    finished: bool,
}

impl<W: ReceiveWindow> Incoming<W> {
    pub(crate) fn new(stream: W::Stream, watch: Watch, window: Arc<W>) -> Self {
        let funding = Funding::Unfunded(None);
        Self {
            stream,
            watch,
            window,
            funding,
            reading: false,
            finished: false,
        }
    }
}

impl<W: ReceiveWindow> http_body::Body for Incoming<W> {
    type Data = Bytes;
    type Error = W::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, W::Error>>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        let (window, stream) = (&this.window, &mut this.stream);
        if !this.reading && this.watch.admitted().is_some() {
            this.reading = true;
            window.reading(stream, true);
        }
        this.funding
            .fund(&this.watch, window.budget(), |keys| window.raise(stream, keys));
        let frame = ready!(W::poll_data(stream, cx));
        this.finished = !matches!(frame, Some(Ok(_)));
        Poll::Ready(frame.map(|data| data.map(Frame::data)))
    }

    fn is_end_stream(&self) -> bool {
        self.finished || W::is_end_stream(&self.stream)
    }
}

impl<W: ReceiveWindow> Drop for Incoming<W> {
    fn drop(&mut self) {
        if self.reading {
            self.window.reading(&mut self.stream, false);
        }
    }
}

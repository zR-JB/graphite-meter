//! The receive window of an HTTP/2 connection, which admitted uploads fund.

use crate::{app::App, exchange::Watch, limits::Hold, lock, peer::ClientKeys, transport::body::Funding};
use bytes::Bytes;
use h2::RecvStream;
use http_body::Frame;
use std::{
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, ready},
};

/// The connection window until an admitted upload reads.
const DEFAULT_WINDOW: u32 = 65_535;
/// Go's connection receive window, raised while admitted uploads read.
const WINDOW: u32 = 16 << 20;

/// The connection's receive window: 64 KiB until an admitted upload reads, then 16 MiB while one reads, within the
/// share of the client that first raised it, which holds the credit until the connection ends.
pub(super) struct Window {
    app: Arc<App>,
    credit: Mutex<Option<Hold>>,
    /// Uploads reading at the raised window.
    uploads: AtomicUsize,
    raised: AtomicBool,
}

impl Window {
    pub(super) fn new(app: Arc<App>) -> Self {
        Self {
            app,
            credit: Mutex::default(),
            uploads: AtomicUsize::new(0),
            raised: AtomicBool::new(false),
        }
    }

    /// Whether an upload of `keys` reads at the raised window, raising it for the first.
    fn fund(&self, stream: &mut RecvStream, keys: &ClientKeys) -> bool {
        if self.uploads.fetch_add(1, Ordering::Relaxed) > 0 || self.raise(stream, keys) {
            return true;
        }
        self.uploads.fetch_sub(1, Ordering::Relaxed);
        false
    }

    fn raise(&self, stream: &mut RecvStream, keys: &ClientKeys) -> bool {
        let mut credit = lock(&self.credit);
        if credit.is_none() {
            *credit = self.app.window_credit(keys, (WINDOW - DEFAULT_WINDOW) as usize);
        }
        if credit.is_some() && stream.flow_control().set_target_connection_window_size(WINDOW) {
            self.raised.store(true, Ordering::Relaxed);
            return true;
        }
        if !self.raised.load(Ordering::Relaxed) {
            *credit = None;
        }
        false
    }

    /// Whether the window was raised, which the connection's credit then holds until it ends.
    pub(super) fn raised(&self) -> bool {
        self.raised.load(Ordering::Relaxed)
    }

    /// An upload reading at the raised window ended; the last lowers it again.
    fn release(&self, stream: &mut RecvStream) {
        if self.uploads.fetch_sub(1, Ordering::Relaxed) == 1 {
            stream.flow_control().set_target_connection_window_size(DEFAULT_WINDOW);
        }
    }
}

/// A request body, which funds the raised window as `Funding` rules; under pressure, or past its client's share, an
/// upload reads at the current window.
pub(super) struct Incoming {
    stream: RecvStream,
    watch: Watch,
    window: Arc<Window>,
    funding: Funding,
}

impl Incoming {
    pub(super) fn new(stream: RecvStream, watch: Watch, window: Arc<Window>) -> Self {
        Self { stream, watch, window, funding: Funding::Unfunded(None) }
    }
}

impl http_body::Body for Incoming {
    type Data = Bytes;
    type Error = h2::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, h2::Error>>> {
        let this = self.get_mut();
        let (window, stream) = (&this.window, &mut this.stream);
        this.funding
            .fund(&this.watch, window.app.budget(), |keys| window.fund(stream, keys));
        let data = ready!(this.stream.poll_data(cx));
        if let Some(Ok(data)) = &data {
            this.stream.flow_control().release_capacity(data.len())?;
        }
        Poll::Ready(data.map(|data| data.map(Frame::data)))
    }

    fn is_end_stream(&self) -> bool {
        self.stream.is_end_stream()
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        if let Funding::Funded = self.funding {
            self.window.release(&mut self.stream);
        }
    }
}

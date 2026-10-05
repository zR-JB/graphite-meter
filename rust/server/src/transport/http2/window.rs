//! The receive window of an HTTP/2 connection, which admitted uploads fund.

use crate::{
    app::App,
    limits::{Budget, Hold},
    lock,
    peer::ClientKeys,
    transport::window::ReceiveWindow,
};
use bytes::Bytes;
use h2::RecvStream;
use std::{
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

    /// Whether the window was raised, which the connection's credit then holds until it ends.
    pub(super) fn raised(&self) -> bool {
        self.raised.load(Ordering::Relaxed)
    }
}

/// Under pressure, or past its client's share, an upload reads at the current window.
impl ReceiveWindow for Window {
    type Stream = RecvStream;
    type Error = h2::Error;

    fn budget(&self) -> &Budget {
        self.app.budget()
    }

    fn raise(&self, stream: &mut RecvStream, keys: &ClientKeys) -> bool {
        if self.uploads.fetch_add(1, Ordering::Relaxed) > 0 {
            return true;
        }
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
        self.uploads.fetch_sub(1, Ordering::Relaxed);
        false
    }

    /// The last upload reading at the raised window lowers it again.
    fn release(&self, stream: &mut RecvStream) {
        if self.uploads.fetch_sub(1, Ordering::Relaxed) == 1 {
            stream.flow_control().set_target_connection_window_size(DEFAULT_WINDOW);
        }
    }

    fn poll_data(stream: &mut RecvStream, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, h2::Error>>> {
        let data = ready!(stream.poll_data(cx));
        if let Some(Ok(data)) = &data
            && let Err(error) = stream.flow_control().release_capacity(data.len())
        {
            return Poll::Ready(Some(Err(error)));
        }
        Poll::Ready(data)
    }

    fn is_end_stream(stream: &RecvStream) -> bool {
        stream.is_end_stream()
    }
}

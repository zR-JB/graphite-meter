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
/// The connection window while admitted uploads read.
const WINDOW: u32 = 16 << 20;

/// The connection's receive window: 64 KiB until an admitted upload reads, then 16 MiB until the last admitted upload
/// stops reading, within the share of the client that first raised it, which holds the credit until the connection
/// ends.
pub(super) struct Window {
    app: Arc<App>,
    credit: Mutex<Option<Hold>>,
    /// Admitted uploads reading, funded or not: running transfers keep the window they read at.
    readers: AtomicUsize,
    raised: AtomicBool,
}

impl Window {
    pub(super) fn new(app: Arc<App>) -> Self {
        Self {
            app,
            credit: Mutex::default(),
            readers: AtomicUsize::new(0),
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

    /// The last admitted upload to stop reading lowers the window again.
    fn reading(&self, stream: &mut RecvStream, reading: bool) {
        if reading {
            self.readers.fetch_add(1, Ordering::Relaxed);
        } else if self.readers.fetch_sub(1, Ordering::Relaxed) == 1 {
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

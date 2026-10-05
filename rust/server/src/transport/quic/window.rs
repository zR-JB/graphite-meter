//! A QUIC connection's windows: the receive window an admitted upload funds, and the send window that follows demand.

use super::budget::ConnectionBudget;
use crate::{
    app::App,
    exchange::Watch,
    limits::{Budget, Pressure},
    peer::ClientKeys,
    transport::body::Funding,
};
use bytes::Bytes;
use graphite_meter_http3::{self as http3, RecvHalf};
use graphite_meter_net::quic::{MAX_SEND_WINDOW, MIN_SEND_WINDOW, RECEIVE_WINDOW};
use http_body::Frame;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, ready},
    time::Duration,
};
use tokio::time::Instant;

/// The least growth of a send window worth a change.
const SEND_WINDOW_STEP: u64 = 256 << 10;
/// A send window shrinks after its demand stayed low this long.
const SEND_WINDOW_SHRINK_DELAY: Duration = Duration::from_secs(1);

/// The receive window: 64 KiB until an admitted upload reads, then autotuning from 768 KiB to 48 MiB on the credit the
/// first funded upload reserves within its client's share, which the connection keeps until it is gone.
pub(super) struct Window {
    app: Arc<App>,
    quic: noq::Connection,
    budget: Arc<ConnectionBudget>,
}

impl Window {
    pub(super) fn new(app: Arc<App>, quic: noq::Connection, budget: Arc<ConnectionBudget>) -> Self {
        Self { app, quic, budget }
    }

    /// Whether an upload of `keys` reads at the raised window, raising it for the first.
    fn fund(&self, keys: &ClientKeys) -> bool {
        let reserved = self.budget.reserve(&self.app, keys);
        if reserved {
            self.quic.set_receive_window(RECEIVE_WINDOW.into());
        }
        reserved
    }

    /// Whether the window was raised.
    pub(super) fn raised(&self) -> bool {
        self.budget.reserved()
    }
}

/// A request body, which funds the raised window as `Funding` rules.
pub(super) struct Incoming {
    stream: RecvHalf,
    watch: Watch,
    window: Arc<Window>,
    funding: Funding,
    finished: bool,
}

impl Incoming {
    pub(super) fn new(stream: RecvHalf, watch: Watch, window: Arc<Window>) -> Self {
        Self {
            stream,
            watch,
            window,
            funding: Funding::Unfunded(None),
            finished: false,
        }
    }
}

impl http_body::Body for Incoming {
    type Data = Bytes;
    type Error = http3::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, http3::Error>>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        let window = &this.window;
        this.funding
            .fund(&this.watch, window.app.budget(), |keys| window.fund(keys));
        let frame = ready!(this.stream.poll_data(cx))
            .transpose()
            .map(|data| data.map(Frame::data));
        this.finished = !matches!(frame, Some(Ok(_)));
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.finished
    }
}

/// The send window: 2 MiB, grown towards two observed bandwidth-delay products up to 16 MiB while the budget has
/// headroom, and back to 2 MiB after a second of low demand or once no request runs.
pub(super) struct SendWindow {
    limit: u64,
    /// When the path's sent bytes were last read, and their count.
    last: Option<(Instant, u64)>,
    low_since: Option<Instant>,
}

impl Default for SendWindow {
    fn default() -> Self {
        Self { limit: MIN_SEND_WINDOW, last: None, low_since: None }
    }
}

impl SendWindow {
    /// Follows the connection's demand while requests run.
    pub(super) fn tune(&mut self, quic: &noq::Connection, busy: bool, budget: &Budget) {
        if !busy {
            return self.release(quic);
        }
        // The total first, so a send on the first path in between cannot look like another path's traffic.
        let all_sent = quic.stats().udp_tx.bytes;
        let path = quic.path_stats(noq::PathId::ZERO);
        let Some(path) = path.filter(|path| all_sent <= path.udp_tx.bytes) else {
            (self.last, self.low_since) = (None, None);
            return;
        };
        let (now, sent) = (Instant::now(), path.udp_tx.bytes);
        if let Some((last, previous)) = self.last {
            let target = desired(sent.saturating_sub(previous), path.rtt, now.duration_since(last));
            if target == MIN_SEND_WINDOW && self.limit != MIN_SEND_WINDOW {
                let since = *self.low_since.get_or_insert(now);
                if now.duration_since(since) >= SEND_WINDOW_SHRINK_DELAY {
                    self.release(quic);
                }
            } else {
                self.low_since = None;
                self.grow(quic, target, budget);
            }
        }
        self.last = Some((now, sent));
    }

    fn grow(&mut self, quic: &noq::Connection, target: u64, budget: &Budget) {
        let granted = target.min(MAX_SEND_WINDOW).saturating_sub(self.limit);
        if granted >= SEND_WINDOW_STEP && budget.pressure() != Pressure::HoldBack {
            self.limit += granted;
            quic.set_send_window(self.limit);
        }
    }

    fn release(&mut self, quic: &noq::Connection) {
        (self.last, self.low_since) = (None, None);
        if self.limit != MIN_SEND_WINDOW {
            self.limit = MIN_SEND_WINDOW;
            quic.set_send_window(MIN_SEND_WINDOW);
        }
    }
}

/// Two bandwidth-delay products of `sent` bytes over `elapsed`, within the send window's bounds: enough for a new path
/// to grow on fast links without filling the bottleneck's queue.
fn desired(sent: u64, rtt: Duration, elapsed: Duration) -> u64 {
    let Some(demand) = u128::from(sent)
        .saturating_mul(rtt.as_nanos())
        .saturating_mul(2)
        .checked_div(elapsed.as_nanos())
    else {
        return MIN_SEND_WINDOW;
    };
    demand.clamp(u128::from(MIN_SEND_WINDOW), u128::from(MAX_SEND_WINDOW)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_send_window_wants_two_bandwidth_delay_products_within_its_bounds() {
        let millis = Duration::from_millis;
        assert_eq!(desired(16 << 20, millis(50), millis(250)), 6_710_886);
        assert_eq!(desired(16 << 20, millis(100), millis(100)), MAX_SEND_WINDOW);
        assert_eq!(desired(1 << 20, millis(1), millis(250)), MIN_SEND_WINDOW);
        assert_eq!(desired(1 << 20, millis(1), Duration::ZERO), MIN_SEND_WINDOW, "no time observed");
    }
}

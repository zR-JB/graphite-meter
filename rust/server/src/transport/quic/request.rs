//! HTTP/3 requests: bounded until admitted, bodies fund the window, 16 KiB paced reply frames, CONNECT to WebTransport.

use super::window::Window;
use crate::{
    app::{App, Connection, Outcome},
    exchange::{Exchange, Watch},
    transport::{
        body::{Aborted, Body, ReplyBound, Sink, pump, within},
        webtransport::{self, ANSWER_BOUND},
        window::{Funding, Incoming},
    },
};
use bytes::Bytes;
use graphite_meter_http3::{self as http3, Code, RequestStream, SendHalf, server};
use http::{Method, Request, Response, response::Parts};
use http_body::Body as _;
use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

const FRAME_BYTES: usize = 16 << 10;
/// Large replies past this many on one connection yield to their siblings after each frame.
const FAIRNESS_REPLIES: usize = 16;
/// Replies larger than this count toward a connection's fairness bound.
const LARGE_REPLY_BYTES: u64 = 1 << 20;
/// The shortest wait of a large reply for its path.
const PACE: Duration = Duration::from_millis(1);

/// What a connection's requests share.
#[derive(Clone)]
pub(super) struct Requests {
    app: Arc<App>,
    connection: Connection,
    window: Arc<Window>,
    quic: noq::Connection,
    large: Arc<AtomicUsize>,
}

impl Requests {
    pub(super) fn new(app: Arc<App>, connection: Connection, window: Arc<Window>, quic: noq::Connection) -> Self {
        Self { app, connection, window, quic, large: Arc::default() }
    }

    /// One request: reset when it expires unadmitted or its reply ends unwritten; admitted work once a poll sees it.
    pub(super) fn serve(&self, request: server::Request) -> impl Future<Output = ()> + Send + use<> {
        let requests = self.clone();
        async move {
            let exchange = Exchange::start();
            let watch = exchange.watch();
            let served = requests.respond(request, exchange, &watch);
            watch.bounded(served, &requests.connection.work).await;
        }
    }

    /// Answers a request; a stream dropped unfinished is reset.
    async fn respond(&self, request: server::Request, exchange: Exchange, watch: &Watch) {
        let Ok((head, stream)) = request.resolve().await else {
            return;
        };
        if head.method() == Method::CONNECT {
            return self.connect(head, stream, exchange, watch).await;
        }
        let (send, recv) = stream.split();
        let bodiless = head.method() == Method::HEAD;
        let request = head.map(|()| Incoming::new(recv, watch.clone(), self.window.clone()));
        let Outcome::Response(response) = self.app.handle(request, &self.connection, exchange).await else {
            return;
        };
        let large = !bodiless
            && response
                .body()
                .size_hint()
                .upper()
                .is_some_and(|bytes| bytes > LARGE_REPLY_BYTES);
        let mut reply = Reply {
            send,
            large: large.then(|| Large::new(&self.large, &self.quic)),
            stopped: None,
        };
        let _ = pump(&mut reply, response, bodiless).await;
    }

    /// A CONNECT: its WebTransport session, or the app's answer within `ANSWER_BOUND`.
    async fn connect(&self, head: Request<()>, stream: RequestStream, exchange: Exchange, watch: &Watch) {
        let request = head.map(|()| Body::empty());
        match self.app.handle(request, &self.connection, exchange).await {
            Outcome::WebTransport(response, lane, plan) => {
                let (window, mut funding) = (&self.window, Funding::Unfunded(None));
                let fund = || {
                    funding.fund(watch, self.app.budget(), |keys| window.fund(keys));
                    matches!(funding, Funding::Funded)
                };
                webtransport::serve(stream, response.into_parts().0.headers, lane, plan, fund).await;
            }
            Outcome::Response(response) => {
                let (send, _recv) = stream.split();
                let mut reply = Reply { send, large: None, stopped: None };
                let _ = tokio::time::timeout(ANSWER_BOUND, pump(&mut reply, response, false)).await;
            }
            Outcome::WebSocket(..) | Outcome::Abort => {}
        }
    }
}

type Stopped = Pin<Box<dyn Future<Output = Result<Option<Code>, http3::Error>> + Send>>;

/// An HTTP/3 stream's reply.
struct Reply {
    send: SendHalf,
    large: Option<Large>,
    /// The peer's STOP_SENDING, watched while the body waits.
    stopped: Option<Stopped>,
}

impl Sink for Reply {
    async fn head(&mut self, head: Parts, end: bool, bound: &mut ReplyBound) -> Result<(), Aborted> {
        within(bound, self.send.send_response(Response::from_parts(head, ()))).await?;
        if end {
            within(bound, self.send.finish()).await?;
        }
        Ok(())
    }

    async fn data(&mut self, mut data: Bytes, last: bool, bound: &mut ReplyBound) -> Result<(), Aborted> {
        while !data.is_empty() {
            if let Some(large) = &self.large {
                within(bound, large.paced()).await?;
            }
            let frame = data.split_to(data.len().min(FRAME_BYTES));
            within(bound, self.send.send_data(frame)).await?;
            bound.progressed();
            if self.large.as_ref().is_some_and(Large::crowded) {
                tokio::task::yield_now().await;
            }
        }
        if last {
            within(bound, self.send.finish()).await?;
        }
        Ok(())
    }

    async fn end(&mut self, bound: &mut ReplyBound) -> Result<(), Aborted> {
        within(bound, self.send.finish()).await
    }

    fn poll_reset(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let send = &self.send;
        let stopped = self.stopped.get_or_insert_with(|| Box::pin(send.stopped()));
        stopped.as_mut().poll(cx).map(drop)
    }
}

/// A large reply on its connection.
struct Large {
    count: Arc<AtomicUsize>,
    quic: noq::Connection,
}

impl Large {
    fn new(count: &Arc<AtomicUsize>, quic: &noq::Connection) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self { count: count.clone(), quic: quic.clone() }
    }

    /// Only a crowded connection needs a handoff after each frame; on others it would halve throughput.
    fn crowded(&self) -> bool {
        self.count.load(Ordering::Relaxed) > FAIRNESS_REPLIES
    }

    /// Waits while unacknowledged data exceeds the path backlog: noq charges credit on write, so bulk starves others.
    async fn paced(&self) -> Result<(), Infallible> {
        while let Some(path) = self.quic.path_stats(noq::PathId::ZERO) {
            if self.quic.send_buffered_bytes() <= backlog(path.cwnd, path.rtt) {
                break;
            }
            tokio::time::sleep(pause(path.rtt)).await;
        }
        Ok(())
    }
}

impl Drop for Large {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A large reply's wait for its path: a quarter round trip, and at least `PACE`.
fn pause(rtt: Duration) -> Duration {
    PACE.max(rtt / 4)
}

/// Eight congestion windows, more where four pauses outlast a round trip: busy paths, backlog below peer credit.
fn backlog(cwnd: u64, rtt: Duration) -> u64 {
    let span = rtt.max(4 * pause(rtt)).as_nanos();
    let bytes = u128::from(cwnd).saturating_mul(8 * span) / rtt.as_nanos().max(1);
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backlog_is_eight_windows_scaled_to_four_pauses() {
        let micros = Duration::from_micros;
        assert_eq!(backlog(30_000, micros(90_000)), 240_000, "a slow path");
        assert_eq!(backlog(300_000, micros(200)), 48_000_000, "loopback");
    }
}

//! HTTP/3 requests: each within its exchange bound until admitted, its body funding the connection's window, and its
//! reply pumped in 16 KiB frames that yield to siblings on a crowded connection; a CONNECT opens a WebTransport session.

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
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

const FRAME_BYTES: usize = 16 << 10;
/// Large replies past this many on one connection yield to their siblings after each frame.
const FAIRNESS_REPLIES: usize = 16;
/// Replies larger than this count toward a connection's fairness bound.
const LARGE_REPLY_BYTES: u64 = 1 << 20;

/// What a connection's requests share.
#[derive(Clone)]
pub(super) struct Requests {
    app: Arc<App>,
    connection: Connection,
    window: Arc<Window>,
    large: Arc<AtomicUsize>,
}

impl Requests {
    pub(super) fn new(app: Arc<App>, connection: Connection, window: Arc<Window>) -> Self {
        Self { app, connection, window, large: Arc::default() }
    }

    /// One request: reset when its exchange expires unadmitted or its reply ends unwritten, and counted as admitted
    /// work from the poll that sees it admitted until it ends.
    pub(super) fn serve(&self, request: server::Request) -> impl Future<Output = ()> + Send + use<> {
        let requests = self.clone();
        async move {
            let exchange = Exchange::start();
            let watch = exchange.watch();
            let served = requests.respond(request, exchange, &watch);
            tokio::select! {
                biased;
                () = watch.counted(served, &requests.connection.work) => {}
                () = watch.expired() => {}
            }
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
            large: large.then(|| Large::new(&self.large)),
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
struct Large(Arc<AtomicUsize>);

impl Large {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(count.clone())
    }

    /// Only a crowded connection needs a handoff after each frame; on others it would halve throughput.
    fn crowded(&self) -> bool {
        self.0.load(Ordering::Relaxed) > FAIRNESS_REPLIES
    }
}

impl Drop for Large {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

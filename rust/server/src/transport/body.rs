//! The response body every transport writes, a document, a download or a progress feed, the rules of writing it
//! within its bound, and the pump that writes it to HTTP/2 and HTTP/3 streams.

use crate::{
    engine::{DownloadSource, ProgressFeed, download::BLOCK_BYTES},
    exchange::EXCHANGE_BOUND,
    lane::Lane,
};
use bytes::Bytes;
use futures_util::{FutureExt, Stream, future::Fuse, stream};
use graphite_meter_proto::lane::LaneEnding;
use http::{Response, response::Parts};
use http_body::{Frame, SizeHint};
use std::{
    error::Error,
    fmt,
    future::{Future, poll_fn},
    io, mem,
    pin::{Pin, pin},
    task::{Context, Poll, ready},
};
use tokio::time::{Instant, Sleep, sleep_until};

/// A reply's body and what bounds writing it.
pub struct Body {
    content: Content,
    bound: Option<Bound>,
}

/// What bounds writing a reply; the app bounds every reply it hands a transport.
#[derive(Debug, Clone)]
pub enum Bound {
    /// The reply is aborted unless written by then: an unadmitted exchange's deadline, or an admitted one's
    /// for its last answer.
    Until(Instant),
    /// Admitted work: written bytes are the lane's movement, its finish follows delivery, other endings abort.
    Lane(Lane),
}

/// A reply ended before its last byte: an HTTP/1 connection closes, an HTTP/2 or HTTP/3 stream resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aborted;

impl fmt::Display for Aborted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("reply aborted")
    }
}

impl Error for Aborted {}

impl From<Aborted> for io::Error {
    fn from(_: Aborted) -> Self {
        io::ErrorKind::ConnectionAborted.into()
    }
}

enum Content {
    /// A document in one frame; empty once sent.
    Full(Bytes),
    Download(DownloadSource),
    Feed(Pin<Box<dyn Stream<Item = Bytes> + Send>>),
}

impl Body {
    pub fn empty() -> Self {
        Self::full(Bytes::new())
    }

    pub fn full(bytes: impl Into<Bytes>) -> Self {
        Self { content: Content::Full(bytes.into()), bound: None }
    }

    pub fn download(source: DownloadSource) -> Self {
        Self { content: Content::Download(source), bound: None }
    }

    /// A progress feed's lines until it ends.
    pub fn feed(feed: ProgressFeed) -> Self {
        let lines = stream::unfold(feed, |mut feed| async move { feed.next().await.map(|line| (line, feed)) });
        Self { content: Content::Feed(Box::pin(lines)), bound: None }
    }

    /// The body as `lane`'s work, which ends with it.
    pub fn with_lane(self, lane: Lane) -> Self {
        Self { bound: Some(Bound::Lane(lane)), ..self }
    }

    pub fn until(self, deadline: Instant) -> Self {
        Self { bound: Some(Bound::Until(deadline)), ..self }
    }

    /// Bounds the reply by `deadline` unless it is bound already.
    pub(crate) fn bound_by_default(&mut self, deadline: Instant) {
        self.bound.get_or_insert(Bound::Until(deadline));
    }
}

impl http_body::Body for Body {
    type Data = Bytes;
    type Error = Aborted;

    /// Each frame first checks the lane, decided without waiting, so a reader that never blocks sees it end.
    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Aborted>>> {
        let this = self.get_mut();
        if let Some(Bound::Lane(lane)) = &this.bound
            && lane.due().is_some_and(|ending| ending != LaneEnding::Finished)
        {
            return Poll::Ready(Some(Err(Aborted)));
        }
        let data = match &mut this.content {
            Content::Full(bytes) => (!bytes.is_empty()).then(|| mem::take(bytes)),
            Content::Download(source) => source.next(BLOCK_BYTES),
            Content::Feed(lines) => {
                let line = ready!(lines.as_mut().poll_next(cx));
                if line.is_none() {
                    this.content = Content::Full(Bytes::new());
                }
                line
            }
        };
        Poll::Ready(data.map(|data| Ok(Frame::data(data))))
    }

    fn is_end_stream(&self) -> bool {
        match &self.content {
            Content::Full(bytes) => bytes.is_empty(),
            Content::Download(source) => source.remaining() == 0,
            Content::Feed(_) => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.content {
            Content::Full(bytes) => SizeHint::with_exact(bytes.len() as u64),
            Content::Download(source) => SizeHint::with_exact(source.remaining()),
            Content::Feed(_) => SizeHint::default(),
        }
    }
}

/// Writing one reply within its bound, as every transport drives it: an `Until` reply is aborted unwritten at its
/// deadline; a lane's reply moves the lane with each write, so the lane's idle bound is its 30 s write stall.
pub struct ReplyBound(Rule);

enum Rule {
    Until(Pin<Box<Sleep>>),
    Lane(Lane, Fuse<Ended>),
}

type Ended = Pin<Box<dyn Future<Output = LaneEnding> + Send>>;

impl ReplyBound {
    /// The bound of `body`; one the app left unbound has the exchange bound from now.
    pub fn of(body: &Body) -> Self {
        match &body.bound {
            Some(Bound::Lane(lane)) => {
                let ending = lane.clone();
                let ended: Ended = Box::pin(async move { ending.ended().await });
                Self(Rule::Lane(lane.clone(), ended.fuse()))
            }
            Some(Bound::Until(deadline)) => Self(Rule::Until(Box::pin(sleep_until(*deadline)))),
            None => Self(Rule::Until(Box::pin(sleep_until(Instant::now() + EXCHANGE_BOUND)))),
        }
    }

    /// Before each write: whether the reply must end now; registers `cx` for an `Until` deadline.
    pub fn check(&mut self, cx: &mut Context<'_>) -> Result<(), Aborted> {
        let aborted = match &mut self.0 {
            Rule::Until(deadline) => deadline.as_mut().poll(cx).is_ready(),
            Rule::Lane(lane, _) => lane.ending().is_some_and(|ending| ending != LaneEnding::Finished),
        };
        if aborted { Err(Aborted) } else { Ok(()) }
    }

    /// While the transport cannot write: registers `cx` for the lane's ending, which then aborts the reply.
    pub fn blocked(&mut self, cx: &mut Context<'_>) -> Result<(), Aborted> {
        match &mut self.0 {
            Rule::Lane(_, ended) => match ended.poll_unpin(cx) {
                Poll::Ready(ending) if ending != LaneEnding::Finished => Err(Aborted),
                _ => Ok(()),
            },
            Rule::Until(_) => Ok(()),
        }
    }

    /// Bytes of the reply were written.
    pub fn progressed(&self) {
        if let Rule::Lane(lane, _) = &self.0 {
            lane.moved();
        }
    }

    /// The reply's last byte was written: its lane finishes unless another ending came first.
    pub fn delivered(&self) -> Result<(), Aborted> {
        match &self.0 {
            Rule::Lane(lane, _) if lane.finish() != LaneEnding::Finished => Err(Aborted),
            _ => Ok(()),
        }
    }
}

/// An HTTP/2 or HTTP/3 stream a reply is pumped into.
pub(crate) trait Sink {
    /// Writes the head; `end` when nothing follows it.
    async fn head(&mut self, head: Parts, end: bool, bound: &mut ReplyBound) -> Result<(), Aborted>;

    /// Writes non-empty `data` as flow control admits it, ending the stream after it when `last`; each frame
    /// written is progress.
    async fn data(&mut self, data: Bytes, last: bool, bound: &mut ReplyBound) -> Result<(), Aborted>;

    /// Ends the stream after a body whose last data did not.
    async fn end(&mut self, bound: &mut ReplyBound) -> Result<(), Aborted>;

    /// Ready once the peer abandoned the reply.
    fn poll_reset(&mut self, cx: &mut Context<'_>) -> Poll<()>;
}

/// Writes a reply within its bound: its head, then, unless it answers HEAD, its data.
pub(crate) async fn pump(sink: &mut impl Sink, response: Response<Body>, head: bool) -> Result<(), Aborted> {
    let (parts, mut body) = response.into_parts();
    let mut bound = ReplyBound::of(&body);
    let end = head || http_body::Body::is_end_stream(&body);
    sink.head(parts, end, &mut bound).await?;
    if end {
        return bound.delivered();
    }
    while let Some(data) = poll_fn(|cx| next_data(cx, &mut body, &mut bound, sink)).await? {
        if data.is_empty() {
            continue;
        }
        let last = http_body::Body::is_end_stream(&body);
        sink.data(data, last, &mut bound).await?;
        if last {
            return bound.delivered();
        }
    }
    sink.end(&mut bound).await?;
    bound.delivered()
}

/// The body's next data; while it has none ready, the reply's bound or the peer's reset ends it.
fn next_data(
    cx: &mut Context<'_>,
    body: &mut Body,
    bound: &mut ReplyBound,
    sink: &mut impl Sink,
) -> Poll<Result<Option<Bytes>, Aborted>> {
    bound.check(cx)?;
    match http_body::Body::poll_frame(Pin::new(body), cx) {
        Poll::Ready(frame) => Poll::Ready(
            frame
                .transpose()
                .map(|frame| frame.map(|frame| frame.into_data().unwrap_or_default())),
        ),
        Poll::Pending => {
            bound.blocked(cx)?;
            sink.poll_reset(cx).map(|()| Err(Aborted))
        }
    }
}

/// Completes `write` unless the reply's bound ends it first.
pub(crate) async fn within<T, E>(
    bound: &mut ReplyBound,
    write: impl Future<Output = Result<T, E>>,
) -> Result<T, Aborted> {
    let mut write = pin!(write);
    poll_fn(|cx| {
        bound.check(cx)?;
        match write.as_mut().poll(cx) {
            Poll::Ready(written) => Poll::Ready(written.map_err(|_| Aborted)),
            Poll::Pending => {
                bound.blocked(cx)?;
                Poll::Pending
            }
        }
    })
    .await
}

impl fmt::Debug for Body {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let content = match &self.content {
            Content::Full(bytes) => format!("{} bytes", bytes.len()),
            Content::Download(source) => format!("download of {} bytes", source.remaining()),
            Content::Feed(_) => "progress feed".into(),
        };
        formatter
            .debug_struct("Body")
            .field("content", &content)
            .field("bound", &self.bound)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::Block,
        exchange::Exchange,
        lane::Work,
        limits::{Budget, Quota},
        peer::ClientKeys,
    };
    use graphite_meter_proto::lane::IDLE_BOUND;
    use http_body::Body as _;
    use http_body_util::BodyExt;
    use std::{future::poll_fn, time::Duration};
    use tokio::time::advance;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn a_download_sends_its_exact_length_in_block_slices() {
        let block = Block::new(&Budget::new(usize::MAX), Default::default()).unwrap();
        let total = BLOCK_BYTES as u64 * 2 + 3;
        let mut body = Body::download(block.source(total));
        assert_eq!(body.size_hint().exact(), Some(total));
        let mut frames = Vec::new();
        while let Some(frame) = body.frame().await {
            frames.push(frame.unwrap().into_data().unwrap().len());
        }
        assert_eq!(frames, [BLOCK_BYTES, BLOCK_BYTES, 3]);
        assert!(body.is_end_stream());
    }

    #[tokio::test]
    async fn a_document_is_one_frame() {
        let body = Body::full("{}");
        assert_eq!((body.size_hint().exact(), body.is_end_stream()), (Some(2), false));
        assert_eq!(body.collect().await.unwrap().to_bytes(), "{}");
        assert!(Body::empty().is_end_stream());
    }

    fn lane(shutdown: &CancellationToken) -> Lane {
        let hold = Quota::new(10, 10).acquire(&ClientKeys::Exempt, 1).unwrap();
        let lifetime = Duration::from_secs(3600);
        Exchange::start().admit(ClientKeys::Exempt, hold, lifetime, &Work::default(), shutdown, None)
    }

    async fn check(bound: &mut ReplyBound) -> Result<(), Aborted> {
        poll_fn(|cx| Poll::Ready(bound.check(cx))).await
    }

    async fn blocked(bound: &mut ReplyBound) -> Result<(), Aborted> {
        poll_fn(|cx| Poll::Ready(bound.blocked(cx))).await
    }

    #[tokio::test(start_paused = true)]
    async fn an_unwritten_reply_is_aborted_at_its_deadline() {
        let body = Body::full("{}").until(Instant::now() + EXCHANGE_BOUND);
        let mut bound = ReplyBound::of(&body);
        advance(EXCHANGE_BOUND - Duration::from_millis(1)).await;
        assert_eq!(check(&mut bound).await, Ok(()));
        advance(Duration::from_millis(1)).await;
        assert_eq!(check(&mut bound).await, Err(Aborted));
    }

    #[tokio::test(start_paused = true)]
    async fn written_bytes_move_a_reply_lane_until_its_delivery_finishes_it() {
        let shutdown = CancellationToken::new();
        let lane = lane(&shutdown);
        let mut bound = ReplyBound::of(&Body::empty().with_lane(lane.clone()));
        advance(IDLE_BOUND - Duration::from_secs(1)).await;
        bound.progressed();
        advance(IDLE_BOUND - Duration::from_secs(1)).await;
        assert_eq!(blocked(&mut bound).await, Ok(()), "a write restarts the stall bound");
        advance(Duration::from_secs(1)).await;
        assert_eq!(blocked(&mut bound).await, Err(Aborted), "thirty seconds without a write");
        assert_eq!((check(&mut bound).await, bound.delivered()), (Err(Aborted), Err(Aborted)));

        let lane = self::lane(&shutdown);
        let bound = ReplyBound::of(&Body::empty().with_lane(lane.clone()));
        assert_eq!(bound.delivered(), Ok(()));
        assert_eq!(lane.ending(), Some(LaneEnding::Finished));
    }

    #[tokio::test]
    async fn a_lane_ending_otherwise_aborts_its_body_at_the_next_frame() {
        let shutdown = CancellationToken::new();
        let block = Block::new(&Budget::new(usize::MAX), Default::default()).unwrap();
        let mut body = Body::download(block.source(BLOCK_BYTES as u64 * 4)).with_lane(lane(&shutdown));
        assert!(body.frame().await.unwrap().is_ok());
        shutdown.cancel();
        assert_eq!(body.frame().await.unwrap().unwrap_err(), Aborted);
    }
}

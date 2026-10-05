//! The response body every transport writes: a document, a download or a progress feed.

use crate::{
    engine::{DownloadSource, ProgressFeed, download::BLOCK_BYTES},
    lane::Lane,
};
use bytes::Bytes;
use futures_util::{Stream, stream};
use http_body::{Frame, SizeHint};
use std::{
    convert::Infallible,
    fmt, mem,
    pin::Pin,
    task::{Context, Poll, ready},
};

/// A reply's body and, while it is admitted work, the lane that bounds writing it.
pub struct Body {
    content: Content,
    lane: Option<Lane>,
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
        Self { content: Content::Full(bytes.into()), lane: None }
    }

    pub fn download(source: DownloadSource) -> Self {
        Self { content: Content::Download(source), lane: None }
    }

    /// A progress feed's lines until it ends.
    pub fn feed(feed: ProgressFeed) -> Self {
        let lines = stream::unfold(feed, |mut feed| async move { feed.next().await.map(|line| (line, feed)) });
        Self { content: Content::Feed(Box::pin(lines)), lane: None }
    }

    /// The body as `lane`'s work: the lane's ending ends the reply, and the lane ends with it.
    pub fn with_lane(self, lane: Lane) -> Self {
        Self { lane: Some(lane), ..self }
    }

    pub fn lane(&self) -> Option<&Lane> {
        self.lane.as_ref()
    }
}

impl http_body::Body for Body {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = self.get_mut();
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
            .field("lane", &self.lane.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine::Block, limits::Budget};
    use http_body::Body as _;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn a_download_sends_its_exact_length_in_block_slices() {
        let block = Block::new(&Budget::new(usize::MAX)).unwrap();
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
}

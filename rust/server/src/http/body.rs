//! Response bodies and the admitted operations that bound them.
use super::upload::ProgressBody;
use crate::{admission::Permit, meter::Transfer};
use bytes::Bytes;
use http::Response;
use hyper::body::{Body, Frame, SizeHint};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::time::Sleep;

/// Direct callers own capacity through this body. A listener additionally holds
/// the operation until its final bytes flush, or the connection is dropped.
pub(crate) struct ResponseBody {
    content: Content,
    pub(super) operation: Option<Arc<Mutex<Operation>>>,
}

/// What a reply sends.
enum Content {
    /// A document, in one frame.
    Bytes(Bytes),
    /// A download: `remaining` bytes of one immutable random block, repeated instead of allocated per write.
    Download {
        block: Bytes,
        remaining: u64,
        transfer: Option<Transfer>,
    },
    /// Upload progress records, until the upload completes.
    Progress(ProgressBody),
}

impl From<Bytes> for ResponseBody {
    fn from(bytes: Bytes) -> Self {
        Self::new(Content::Bytes(bytes))
    }
}

impl ResponseBody {
    fn new(content: Content) -> Self {
        Self {
            content,
            operation: None,
        }
    }

    pub(super) fn empty() -> Self {
        Bytes::new().into()
    }

    /// A download's `remaining` bytes of `block`, which `transfer` meters.
    pub(super) fn download(block: Bytes, remaining: u64, transfer: Option<Transfer>) -> Self {
        Self::new(Content::Download {
            block,
            remaining,
            transfer,
        })
    }

    pub(super) fn progress(progress: ProgressBody) -> Self {
        Self::new(Content::Progress(progress))
    }

    fn complete(&self) {
        if let Some(operation) = &self.operation {
            operation.lock().expect("operation poisoned").body_complete = true;
        }
    }
}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.is_end_stream() {
            return Poll::Ready(None);
        }
        if let Some(operation) = &self.operation {
            let error = operation.lock().expect("operation poisoned").check(cx).err();
            if let Some(error) = error {
                // A byte body ends with its error; a progress stream reports it again when polled again.
                match &mut self.content {
                    Content::Bytes(bytes) => *bytes = Bytes::new(),
                    Content::Download { remaining, .. } => *remaining = 0,
                    Content::Progress(_) => {}
                }
                return Poll::Ready(Some(Err(error)));
            }
        }
        let frame = match &mut self.content {
            Content::Progress(progress) => {
                let frame = progress.poll_frame(cx);
                if progress.done {
                    self.complete();
                }
                return frame;
            }
            Content::Bytes(bytes) => std::mem::take(bytes),
            Content::Download {
                block,
                remaining,
                transfer,
            } => {
                let length = (*remaining).min(block.len() as u64) as usize;
                *remaining -= length as u64;
                if let Some(transfer) = transfer {
                    transfer.record(length);
                }
                block.slice(..length)
            }
        };
        if self.is_end_stream() {
            self.complete();
        }
        Poll::Ready(Some(Ok(Frame::data(frame))))
    }

    fn is_end_stream(&self) -> bool {
        match &self.content {
            Content::Bytes(bytes) => bytes.is_empty(),
            Content::Download { remaining, .. } => *remaining == 0,
            Content::Progress(progress) => progress.done,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.content {
            Content::Bytes(bytes) => SizeHint::with_exact(bytes.len() as u64),
            Content::Download { remaining, .. } => SizeHint::with_exact(*remaining),
            Content::Progress(progress) if progress.done => SizeHint::with_exact(0),
            Content::Progress(_) => SizeHint::default(),
        }
    }
}

/// Where a multiplexed reply goes: an HTTP/2 stream, or an HTTP/3 request stream.
pub(super) trait DataSink {
    /// Sends the head, as the whole reply when `end`; `true` when that completed the reply.
    async fn head(&mut self, head: Response<()>, end: bool) -> io::Result<bool>;
    /// Writes a prefix of `data`, which ends the reply when `end` and it leaves nothing; `true` when it did.
    async fn data(&mut self, data: &mut Bytes, end: bool) -> io::Result<bool>;
    /// Ends a reply that no write ended.
    async fn finish(&mut self) -> io::Result<()>;
    /// An error once the peer cancels the reply; it is awaited while the body has no data ready.
    async fn cancelled(&mut self) -> io::Error {
        std::future::pending().await
    }
}

/// Writes a reply to `sink`: its head, then, unless it answers HEAD, its body's data as the sink takes it.
pub(super) async fn write_reply(sink: &mut impl DataSink, reply: Response<ResponseBody>, head: bool) -> io::Result<()> {
    let (parts, mut body) = reply.into_parts();
    if sink
        .head(Response::from_parts(parts, ()), head || body.is_end_stream())
        .await?
    {
        return Ok(());
    }
    if !head {
        loop {
            let frame = tokio::select! {
                error = sink.cancelled() => return Err(error),
                frame = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)) => frame,
            };
            let Some(frame) = frame else { break };
            if let Ok(mut data) = frame?.into_data() {
                while !data.is_empty() {
                    if sink.data(&mut data, body.is_end_stream()).await? {
                        return Ok(());
                    }
                }
            }
        }
    }
    sink.finish().await
}

// An operation outlives the body when Hyper has queued its last frame but has
// not flushed it. Keeping both deadline and permit here bounds stalled writes.
pub(super) struct Operation {
    pub(super) permit: Option<Permit>,
    pub(super) deadline: Pin<Box<Sleep>>,
    pub(super) body_complete: bool,
    pub(super) revocation: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    pub(super) revoked: bool,
}

/// The operations one exchange, or one HTTP/1 connection, holds until their bytes are written.
pub(super) type Operations = Arc<Mutex<Vec<Arc<Mutex<Operation>>>>>;

impl Operation {
    pub(super) fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.revoked
            || self
                .revocation
                .as_mut()
                .is_some_and(|ended| ended.as_mut().poll(cx).is_ready())
        {
            self.revoked = true;
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if self.deadline.as_mut().poll(cx).is_ready() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}

pub(super) fn check_operations(operations: &Operations, cx: &mut Context<'_>) -> io::Result<()> {
    for operation in operations.lock().expect("operations poisoned").iter() {
        operation.lock().expect("operation poisoned").check(cx)?;
    }
    Ok(())
}

pub(super) fn holds_permit(operations: &Operations) -> bool {
    let operations = operations.lock().expect("operations poisoned");
    operations
        .iter()
        .any(|operation| operation.lock().expect("operation poisoned").permit.is_some())
}

/// An upload body asks its connection for a wider receive window once its exchange holds a permit, on each read
/// until the connection grants it.
pub(super) struct UploadFunding {
    operations: Operations,
    pub(super) funded: bool,
}

impl UploadFunding {
    pub(super) fn new(operations: Operations) -> Self {
        Self {
            operations,
            funded: false,
        }
    }

    /// While the upload is unfunded and `ready` holds, `grant` is asked with the keys the exchange's permit holds,
    /// which also bound the client's receive credit.
    pub(super) fn fund(&mut self, ready: impl FnOnce() -> bool, grant: impl FnOnce(&[String]) -> bool) {
        if !self.funded
            && ready()
            && let Some(clients) = self.admitted_clients()
        {
            self.funded = grant(&clients);
        }
    }

    fn admitted_clients(&self) -> Option<Vec<String>> {
        let operations = self.operations.lock().expect("operations poisoned");
        operations.iter().find_map(|operation| {
            let operation = operation.lock().expect("operation poisoned");
            operation.permit.as_ref().map(|permit| permit.clients().to_vec())
        })
    }
}

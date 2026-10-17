//! Test fixtures: TLS identities, a delaying, stalling relay and temporary directories.
mod identity;
mod link;
mod scratch;

pub use identity::Identity;
pub use link::{Bottleneck, Link};
pub use scratch::Scratch;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// An HTTP/1.1 head, read byte by byte so nothing after it is consumed; none if the stream ends first.
pub async fn read_head(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await.ok()?);
    }
    String::from_utf8(head).ok()
}

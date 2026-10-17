//! Provides test fixtures, from TLS identities and a delaying, stalling relay to temporary directories.
mod identity;
mod link;
mod scratch;

pub use identity::Identity;
pub use link::{Bottleneck, Link};
pub use scratch::Scratch;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

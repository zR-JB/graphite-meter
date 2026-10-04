//! Test fixtures: TLS identities, a delaying, faulting relay and temporary directories.
mod identity;
mod link;
mod scratch;

pub use identity::Identity;
pub use link::{Fault, Link};
pub use scratch::Scratch;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

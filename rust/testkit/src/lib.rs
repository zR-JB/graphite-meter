//! Test fixtures: TLS identities and a delaying, faulting relay.
mod identity;
mod link;

pub use identity::Identity;
pub use link::{Fault, Link};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

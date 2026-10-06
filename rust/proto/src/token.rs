//! Tokens, upload IDs and socket tickets (`api/discovery.md`, `api/wire.md#socket-credentials`).

use serde::Serialize;

/// A token or upload ID holds at most this many bytes.
pub const MAX_TOKEN_BYTES: usize = 8192;

/// Whether `token` can be a token or upload ID: nonempty and at most 8192 bytes.
pub fn valid(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN_BYTES
}

/// A socket-ticket mint's answer: a single-use token and its expiry in epoch milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SocketTicket {
    pub token: String,
    pub expires: u64,
}

impl SocketTicket {
    /// The answer with authentication off.
    pub fn unauthenticated() -> Self {
        Self { token: String::new(), expires: 0 }
    }
}

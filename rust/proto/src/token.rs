//! Tokens, upload IDs and socket tickets (`api/discovery.md`, `api/wire.md#socket-credentials`).

use serde::Serialize;

const MAX_TOKEN_BYTES: usize = 8192;

/// Whether `token` can be a token or upload ID: nonempty and bounded.
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
    pub fn unauthenticated() -> Self {
        Self {
            token: String::new(),
            expires: 0,
        }
    }
}

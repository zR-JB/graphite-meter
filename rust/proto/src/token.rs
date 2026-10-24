//! Tokens and upload IDs (`api/discovery.md`).

const MAX_TOKEN_BYTES: usize = 8192;

/// Whether `token` can be a token or upload ID: nonempty and bounded.
pub fn valid(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN_BYTES
}

//! Ping processing shared by WebSocket messages and WebTransport datagrams.

use graphite_meter_core::wire::{decode_ping, encode_pong};
use std::time::Instant;

/// Call after receiving a complete message, before awaiting any other work.
///
/// Handling time includes validation, but excludes receive queues, response
/// encoding and sending, matching the Go endpoint's measurement boundary.
/// Malformed messages receive no reply and do not terminate the session.
pub fn reply(message: &[u8]) -> Option<String> {
    let received_at = Instant::now();
    let text = std::str::from_utf8(message).ok()?;
    let id = decode_ping(text).ok()?;
    let handling_nanos = u64::try_from(received_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
    Some(encode_pong(id, handling_nanos))
}

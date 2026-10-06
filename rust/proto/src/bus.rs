//! The latency bus messages (`api/wire.md`): one per WebSocket text message or WebTransport datagram.

use std::str::{FromStr, from_utf8};

/// `PING,<id>`: a client probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ping {
    pub id: u32,
}

/// `PONG,<id>,<handling-ns>`: the reflector's reply and how long it handled the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    pub id: u32,
    pub handling_nanos: u64,
}

impl Ping {
    pub fn encode(self) -> String {
        format!("PING,{}", self.id)
    }

    /// Reads a probe; `None` for a malformed message, which receivers ignore without replying or closing.
    pub fn decode(message: &[u8]) -> Option<Self> {
        let id = from_utf8(message).ok()?.strip_prefix("PING,")?;
        Some(Self { id: decimal(id, 10)? })
    }

    /// The bus's next probe; ids wrap at 2^32.
    pub fn next(self) -> Self {
        Self { id: self.id.wrapping_add(1) }
    }

    /// The reply echoing this probe's id.
    pub fn reply(self, handling_nanos: u64) -> Pong {
        Pong { id: self.id, handling_nanos }
    }
}

impl Pong {
    pub fn encode(self) -> String {
        format!("PONG,{},{}", self.id, self.handling_nanos)
    }

    /// Reads a reply; `None` for a malformed message, never a reply with an invented duration.
    pub fn decode(message: &[u8]) -> Option<Self> {
        let (id, nanos) = from_utf8(message).ok()?.strip_prefix("PONG,")?.split_once(',')?;
        Some(Self { id: decimal(id, 10)?, handling_nanos: decimal(nanos, 20)? })
    }
}

/// A field of digits only, at most `digits` of them, that fits `T`.
fn decimal<T: FromStr>(field: &str, digits: usize) -> Option<T> {
    let digits_only = !field.is_empty() && field.len() <= digits && field.bytes().all(|byte| byte.is_ascii_digit());
    digits_only.then(|| field.parse().ok()).flatten()
}

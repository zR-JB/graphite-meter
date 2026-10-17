//! The latency bus messages (`api/wire.md`): one per WebSocket text message or WebTransport datagram.

use crate::text::decimal;
use std::{fmt::NumBuffer, str::from_utf8};

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
    /// The longest probe: `PING,` and ten digits.
    pub const MAX_BYTES: usize = 15;

    pub fn encode(self) -> String {
        String::from_utf8_lossy(self.write(&mut [0; Self::MAX_BYTES])).into_owned()
    }

    /// The probe's bytes, written into `bytes` without allocating.
    pub fn write(self, bytes: &mut [u8; Self::MAX_BYTES]) -> &[u8] {
        let mut id = NumBuffer::new();
        let id = self.id.format_into(&mut id).as_bytes();
        let (prefix, digits) = bytes.split_at_mut(5);
        prefix.copy_from_slice(b"PING,");
        digits[..id.len()].copy_from_slice(id);
        &bytes[..5 + id.len()]
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
        let (mut id, mut nanos) = (NumBuffer::new(), NumBuffer::new());
        ["PONG,", self.id.format_into(&mut id), ",", self.handling_nanos.format_into(&mut nanos)].concat()
    }

    /// Reads a reply; `None` for a malformed message, never a reply with an invented duration.
    pub fn decode(message: &[u8]) -> Option<Self> {
        let (id, nanos) = from_utf8(message).ok()?.strip_prefix("PONG,")?.split_once(',')?;
        Some(Self { id: decimal(id, 10)?, handling_nanos: decimal(nanos, 20)? })
    }
}

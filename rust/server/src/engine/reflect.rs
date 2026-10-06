//! The latency reflector (`api/wire.md#reflector-handling-time`).

use graphite_meter_proto::bus::Ping;
use std::time::Instant;

/// The PONG for a bus message received at `received`, with the time since; `None` for a malformed message.
pub fn reflect(message: &[u8], received: Instant) -> Option<String> {
    let ping = Ping::decode(message)?;
    let handling = u64::try_from(received.elapsed().as_nanos()).unwrap_or(u64::MAX);
    Some(ping.reply(handling).encode())
}

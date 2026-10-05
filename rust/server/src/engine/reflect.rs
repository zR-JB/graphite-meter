//! The latency reflector (`api/wire.md#reflector-handling-time`).

use graphite_meter_proto::bus::Ping;
use std::time::Instant;

/// The PONG for a bus message whose receive call returned at `received`, with the time spent since; `None` for a
/// malformed message, which gets no reply.
pub fn reflect(message: &[u8], received: Instant) -> Option<String> {
    let ping = Ping::decode(message)?;
    let handling = u64::try_from(received.elapsed().as_nanos()).unwrap_or(u64::MAX);
    Some(ping.reply(handling).encode())
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphite_meter_proto::bus::Pong;
    use std::time::Duration;

    #[test]
    fn a_ping_is_answered_with_its_id_and_the_time_since_it_was_received() {
        let received = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
        let pong = Pong::decode(reflect(b"PING,4294967295", received).unwrap().as_bytes()).unwrap();
        assert_eq!(pong.id, u32::MAX);
        assert!(pong.handling_nanos >= 2_000_000);
        assert!(Duration::from_nanos(pong.handling_nanos) <= received.elapsed());
    }

    #[test]
    fn malformed_messages_get_no_reply() {
        for message in [&b"PING,"[..], b"PING,-1", b"PING,4294967296", b"PONG,1,2", b"PING,1 ", b"\xff"] {
            assert_eq!(reflect(message, Instant::now()), None, "{message:?}");
        }
    }
}

//! Query parameters and URL-encoded forms; transfer parameters clamped (`api/wire.md#webtransport-routes`).

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use std::time::Duration;

/// The transfer size when `bytes=` is missing or unreadable.
const DEFAULT_TRANSFER_BYTES: u64 = 25 << 20;
const MAX_TRANSFER_BYTES: u64 = 64 << 30;
/// Streams a WebTransport lane carries: a download's `streams=` and the uploads read at once; more are stopped.
pub const MAX_LANE_STREAMS: usize = 16;

/// The first value of `name`; pairs holding `;` or a bad escape are left out.
pub fn get(query: Option<&str>, name: &str) -> Option<String> {
    values(query, name).next()
}

pub fn values<'a>(query: Option<&'a str>, name: &'a str) -> impl Iterator<Item = String> + 'a {
    let pairs = query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter_map(pair);
    pairs
        .filter(move |(key, _)| key == name)
        .map(|(_, value)| value)
}

/// A URL-encoded form's fields; `None` when one holds `;` or a bad escape, or a name repeats.
pub fn form(body: &str) -> Option<Vec<(String, String)>> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for text in body.split('&').filter(|text| !text.is_empty()) {
        let (name, value) = pair(text)?;
        if fields.iter().any(|(known, _)| *known == name) {
            return None;
        }
        fields.push((name, value));
    }
    Some(fields)
}

pub fn encode(pairs: &[(&str, &str)]) -> String {
    let pair = |(name, value): &(&str, &str)| format!("{}={}", escape(name), escape(value));
    pairs.iter().map(pair).collect::<Vec<_>>().join("&")
}

/// Unreserved bytes stay, a space becomes `+`, every other byte is percent-encoded.
pub fn escape(text: &str) -> String {
    const ESCAPED: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    utf8_percent_encode(text, ESCAPED)
        .to_string()
        .replace("%20", "+")
}

fn pair(text: &str) -> Option<(String, String)> {
    if text.is_empty() || text.contains(';') {
        return None;
    }
    let (key, value) = text.split_once('=').unwrap_or((text, ""));
    Some((decode(key)?, decode(value)?))
}

fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let escaped = |at: usize| {
        bytes
            .get(at + 1..at + 3)
            .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
    };
    let valid = bytes
        .iter()
        .enumerate()
        .all(|(at, &byte)| byte != b'%' || escaped(at));
    valid.then(|| {
        percent_decode_str(&text.replace('+', " "))
            .decode_utf8_lossy()
            .into_owned()
    })
}

/// `bytes=`: missing, non-numeric or negative reads as the default, and above the maximum as the maximum.
pub fn transfer_bytes(query: Option<&str>) -> u64 {
    match get(query, "bytes").as_deref().and_then(integer) {
        Some(bytes) if bytes >= 0 => (bytes as u64).min(MAX_TRANSFER_BYTES),
        _ => DEFAULT_TRANSFER_BYTES,
    }
}

/// `ms=`: a positive whole number of milliseconds, else none.
pub fn transfer_ms(query: Option<&str>) -> Option<Duration> {
    let ms = get(query, "ms").as_deref().and_then(integer)?;
    (ms > 0).then(|| Duration::from_millis(ms as u64))
}

/// `streams=`: missing, non-numeric or below one reads as one, and above the cap as the cap.
pub fn streams(query: Option<&str>) -> usize {
    let streams = get(query, "streams").as_deref().and_then(integer);
    streams.map_or(1, |streams| {
        streams.clamp(1, MAX_LANE_STREAMS as i64) as usize
    })
}

/// `datagrams=` asks for datagrams unless it spells zero or false.
pub fn datagrams(query: Option<&str>) -> bool {
    flag(query, "datagrams")
}

/// A presence-based switch: set unless it spells zero or false.
pub fn flag(query: Option<&str>, name: &str) -> bool {
    let Some(value) = get(query, name) else {
        return false;
    };
    let value = value.trim();
    match integer(value) {
        Some(number) => number != 0,
        None => !["false", "off", "no"]
            .iter()
            .any(|no| value.eq_ignore_ascii_case(no)),
    }
}

/// A signed decimal integer, saturated to the `i64` range.
fn integer(text: &str) -> Option<i64> {
    use std::num::IntErrorKind::{NegOverflow, PosOverflow};
    match text.parse::<i64>() {
        Ok(number) => Some(number),
        Err(error) if *error.kind() == PosOverflow => Some(i64::MAX),
        Err(error) if *error.kind() == NegOverflow => Some(i64::MIN),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_sizes_durations_and_stream_counts_are_clamped_never_refused() {
        for (query, bytes) in [
            (None, DEFAULT_TRANSFER_BYTES),
            (Some("bytes="), DEFAULT_TRANSFER_BYTES),
            (Some("bytes=ten"), DEFAULT_TRANSFER_BYTES),
            (Some("bytes=-1"), DEFAULT_TRANSFER_BYTES),
            (Some("bytes=-99999999999999999999"), DEFAULT_TRANSFER_BYTES),
            (Some("bytes=99999999999999999999"), MAX_TRANSFER_BYTES),
            (Some("bytes=0"), 0),
            (Some("bytes=%2B7"), 7),
            (Some("bytes=68719476737"), MAX_TRANSFER_BYTES),
        ] {
            assert_eq!(transfer_bytes(query), bytes, "{query:?}");
        }
        for (query, ms) in [
            (None, None),
            (Some("ms=0"), None),
            (Some("ms=-5"), None),
            (Some("ms=soon"), None),
            (Some("ms=50"), Some(Duration::from_millis(50))),
        ] {
            assert_eq!(transfer_ms(query), ms, "{query:?}");
        }
        for (query, streams) in [
            (None, 1),
            (Some("streams=0"), 1),
            (Some("streams=x"), 1),
            (Some("streams=5"), 5),
        ] {
            assert_eq!(super::streams(query), streams, "{query:?}");
        }
        assert_eq!(streams(Some("streams=17")), MAX_LANE_STREAMS);
        assert_eq!(
            streams(Some("streams=99999999999999999999")),
            MAX_LANE_STREAMS
        );
    }

    #[test]
    fn datagrams_are_asked_for_by_presence_unless_zero_or_false() {
        for (query, asked) in [
            (None, false),
            (Some("datagrams"), true),
            (Some("datagrams="), true),
            (Some("datagrams=yes"), true),
            (Some("datagrams=%ff"), true),
            (Some("datagrams=2"), true),
            (Some("datagrams=0"), false),
            (Some("datagrams=00"), false),
            (Some("datagrams=+FALSE+"), false),
            (Some("datagrams=Off"), false),
            (Some("datagrams=no"), false),
        ] {
            assert_eq!(datagrams(query), asked, "{query:?}");
        }
    }
}

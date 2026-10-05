//! Query parameters as Go's `url.Values` reads them, and the transfer parameters, clamped and never refused
//! (`api/wire.md#webtransport-routes`).

use percent_encoding::percent_decode_str;

/// The transfer size when `bytes=` is missing or unreadable.
pub const DEFAULT_TRANSFER_BYTES: u64 = 25 << 20;
pub const MAX_TRANSFER_BYTES: u64 = 64 << 30;
pub const MAX_STREAMS: usize = 16;

/// The first value of `name`; pairs holding `;` or a bad escape are left out.
pub fn get(query: Option<&str>, name: &str) -> Option<String> {
    let mut pairs = query?.split('&').filter_map(pair);
    pairs.find(|(key, _)| key == name).map(|(_, value)| value)
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
    let valid = bytes.iter().enumerate().all(|(at, &byte)| byte != b'%' || escaped(at));
    valid.then(|| {
        percent_decode_str(&text.replace('+', " "))
            .decode_utf8_lossy()
            .into_owned()
    })
}

/// `bytes=`: missing, non-numeric or negative reads as 25 MiB, and above 64 GiB as 64 GiB.
pub fn transfer_bytes(query: Option<&str>) -> u64 {
    match get(query, "bytes").and_then(|value| value.parse::<i64>().ok()) {
        Some(bytes) if bytes >= 0 => (bytes as u64).min(MAX_TRANSFER_BYTES),
        _ => DEFAULT_TRANSFER_BYTES,
    }
}

/// `streams=`: missing, non-numeric or below one reads as one, and above 16 as 16.
pub fn streams(query: Option<&str>) -> usize {
    let streams = get(query, "streams").and_then(|value| value.parse::<i64>().ok());
    streams.map_or(1, |streams| streams.clamp(1, MAX_STREAMS as i64) as usize)
}

/// `datagrams=` asks for datagrams unless it spells zero or false.
pub fn datagrams(query: Option<&str>) -> bool {
    let Some(value) = get(query, "datagrams") else { return false };
    let value = value.trim();
    match value.parse::<i64>() {
        Ok(number) => number != 0,
        Err(_) => !["false", "off", "no"].iter().any(|no| value.eq_ignore_ascii_case(no)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_decode_as_go_reads_them() {
        let query = Some("a=1;b&id=x%2By+z&id=second&bad=%zz&id2=%41");
        assert_eq!(get(query, "id").as_deref(), Some("x+y z"));
        assert_eq!(get(query, "a"), None, "a pair holding ';' is left out");
        assert_eq!(get(query, "bad"), None);
        assert_eq!(get(query, "id2").as_deref(), Some("A"));
        assert_eq!(get(Some("flag"), "flag").as_deref(), Some(""));
        assert_eq!(get(None, "id"), None);
    }

    #[test]
    fn transfer_sizes_and_stream_counts_are_clamped_never_refused() {
        for (query, bytes) in [
            (None, DEFAULT_TRANSFER_BYTES),
            (Some("bytes="), DEFAULT_TRANSFER_BYTES),
            (Some("bytes=ten"), DEFAULT_TRANSFER_BYTES),
            (Some("bytes=-1"), DEFAULT_TRANSFER_BYTES),
            (Some("bytes=99999999999999999999"), DEFAULT_TRANSFER_BYTES),
            (Some("bytes=0"), 0),
            (Some("bytes=%2B7"), 7),
            (Some("bytes=68719476737"), MAX_TRANSFER_BYTES),
        ] {
            assert_eq!(transfer_bytes(query), bytes, "{query:?}");
        }
        for (query, streams) in [(None, 1), (Some("streams=0"), 1), (Some("streams=x"), 1), (Some("streams=5"), 5)] {
            assert_eq!(super::streams(query), streams, "{query:?}");
        }
        assert_eq!(streams(Some("streams=17")), MAX_STREAMS);
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

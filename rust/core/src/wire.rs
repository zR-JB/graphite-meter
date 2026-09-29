//! Text probe frames and upload progress records shared by server and clients.

use serde::de::{Error as _, IgnoredAny, MapAccess, Visitor};
use serde::{Deserializer, Serialize};
use serde_json::{Map, Value};
use std::{collections::HashSet, fmt, time::Duration};

pub const MAX_UPLOAD_COUNTER: u64 = (1 << 53) - 1;
pub const MAX_TRANSFER_BYTES: u64 = 64 << 30;
pub const MAX_WEBTRANSPORT_STREAMS: usize = 16;
/// The published inactivity bound of every lane, per api/wire.md#lane-endings, as Go's `wire.IdleBound`.
pub const IDLE_BOUND: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    MalformedProbe,
    InvalidUploadProgress,
    UploadCounterOutOfRange,
    InvalidReceiverCheckpoint,
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MalformedProbe => "malformed ping protocol message",
            Self::InvalidUploadProgress => "invalid upload progress record",
            Self::UploadCounterOutOfRange => "upload progress counter exceeds exact JSON range",
            Self::InvalidReceiverCheckpoint => "invalid receiver checkpoint counters",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for WireError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    pub id: u32,
    pub handling_nanos: u64,
}

fn decimal<T: std::str::FromStr>(text: &str, max_len: usize) -> Result<T, WireError> {
    if text.is_empty() || text.len() > max_len || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(WireError::MalformedProbe);
    }
    text.parse().map_err(|_| WireError::MalformedProbe)
}

pub fn decode_ping(message: &str) -> Result<u32, WireError> {
    decimal(message.strip_prefix("PING,").ok_or(WireError::MalformedProbe)?, 10)
}

pub fn decode_pong(message: &str) -> Result<Pong, WireError> {
    let payload = message.strip_prefix("PONG,").ok_or(WireError::MalformedProbe)?;
    let (id, handling_nanos) = payload.split_once(',').ok_or(WireError::MalformedProbe)?;
    Ok(Pong {
        id: decimal(id, 10)?,
        handling_nanos: decimal(handling_nanos, 20)?,
    })
}

pub fn encode_ping(id: u32) -> String {
    format!("PING,{id}")
}

pub fn encode_pong(id: u32, handling_nanos: u64) -> String {
    format!("PONG,{id},{handling_nanos}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadProgress {
    Ready,
    Error { message: String, code: String },
    Progress { bytes: u64, nanos: u64 },
    Complete { bytes: u64, nanos: u64 },
}

#[derive(Serialize)]
struct EncodedProgress<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nanos: Option<u64>,
    #[serde(skip_serializing_if = "str::is_empty")]
    message: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    code: &'a str,
}

pub fn encode_upload_progress(event: &UploadProgress) -> Result<String, WireError> {
    let record = match event {
        UploadProgress::Ready => EncodedProgress {
            kind: "ready",
            bytes: None,
            nanos: None,
            message: "",
            code: "",
        },
        UploadProgress::Error { message, code } => EncodedProgress {
            kind: "error",
            bytes: None,
            nanos: None,
            message,
            code,
        },
        UploadProgress::Progress { bytes, nanos } | UploadProgress::Complete { bytes, nanos } => {
            if *bytes > MAX_UPLOAD_COUNTER || *nanos > MAX_UPLOAD_COUNTER {
                return Err(WireError::UploadCounterOutOfRange);
            }
            EncodedProgress {
                kind: if matches!(event, UploadProgress::Progress { .. }) {
                    "progress"
                } else {
                    "complete"
                },
                bytes: Some(*bytes),
                nanos: Some(*nanos),
                message: "",
                code: "",
            }
        }
    };
    serde_json::to_string(&record).map_err(|_| WireError::InvalidUploadProgress)
}

/// Decodes JSON as Go's json/v2 does: [`strict`] refuses what it refuses, and serde skips unknown
/// members without converting their numbers or bounding their nesting.
pub fn decode_json<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, serde_json::Error> {
    strict(data)?;
    serde_json::from_slice(data)
}

/// As Go's DecodeUploadProgress, only the members a record's type uses are read.
pub fn decode_upload_progress(data: &[u8]) -> Result<UploadProgress, WireError> {
    let members = |names: &[&str]| decode_members(data, names).map_err(|_| WireError::InvalidUploadProgress);
    let kind = match members(&["type"])?.remove("type") {
        Some(Value::String(kind)) => kind,
        _ => return Err(WireError::InvalidUploadProgress),
    };
    let event = match kind.as_str() {
        "ready" => UploadProgress::Ready,
        "error" => {
            let mut fields = members(&["message", "code"])?;
            let detail = |value: Option<Value>| -> Result<String, WireError> {
                match value {
                    None => Ok(String::new()),
                    Some(Value::String(text)) => Ok(text),
                    _ => Err(WireError::InvalidUploadProgress),
                }
            };
            UploadProgress::Error {
                message: detail(fields.remove("message"))?,
                code: detail(fields.remove("code"))?,
            }
        }
        "progress" | "complete" => {
            let mut fields = members(&["bytes", "nanos"])?;
            let bytes = counter(fields.remove("bytes"))?;
            let nanos = counter(fields.remove("nanos"))?;
            if kind == "progress" {
                UploadProgress::Progress { bytes, nanos }
            } else {
                UploadProgress::Complete { bytes, nanos }
            }
        }
        _ => return Err(WireError::InvalidUploadProgress),
    };
    Ok(event)
}

fn counter(value: Option<Value>) -> Result<u64, WireError> {
    let Some(Value::Number(value)) = value else {
        return Err(WireError::InvalidUploadProgress);
    };
    let number = value.as_f64().ok_or(WireError::InvalidUploadProgress)?;
    if !number.is_finite() || number < 0.0 || number > MAX_UPLOAD_COUNTER as f64 || number.trunc() != number {
        return Err(WireError::InvalidUploadProgress);
    }
    Ok(number as u64)
}

/// What Go's json/v2 refuses and serde lets through: a member name twice in one object at any
/// depth, invalid UTF-8, an escape that is no character, and nesting past Go's bound. The scan is
/// exact for valid JSON; serde_json refuses the rest after it.
fn strict(data: &[u8]) -> Result<(), serde_json::Error> {
    let text = std::str::from_utf8(data).map_err(serde_json::Error::custom)?;
    // Each open container: an object's member names so far, or `None` for an array.
    let mut open: Vec<Option<HashSet<String>>> = Vec::new();
    let (bytes, mut at, mut previous) = (text.as_bytes(), 0, b' ');
    while let Some(&byte) = bytes.get(at) {
        match byte {
            // Go's json/v2 bound on nesting.
            b'{' | b'[' if open.len() == 10_000 => return Err(serde_json::Error::custom("JSON nested too deep")),
            b'{' => open.push(Some(HashSet::new())),
            b'[' => open.push(None),
            b'}' | b']' => drop(open.pop()),
            b'"' => {
                let mut strings = serde_json::Deserializer::from_str(&text[at..]).into_iter::<String>();
                let string = strings.next().transpose()?.unwrap_or_default();
                at += strings.byte_offset() - 1;
                let names = open.last_mut().and_then(Option::as_mut);
                if matches!(previous, b'{' | b',') && names.is_some_and(|names| !names.insert(string)) {
                    return Err(serde_json::Error::custom("duplicate JSON member"));
                }
            }
            _ => {}
        }
        previous = if byte.is_ascii_whitespace() { previous } else { byte };
        at += 1;
    }
    Ok(())
}

/// The members `names` lists of the object `data` holds, checked as [`decode_json`] checks it.
fn decode_members(data: &[u8], names: &[&str]) -> Result<Map<String, Value>, serde_json::Error> {
    strict(data)?;
    let mut deserializer = serde_json::Deserializer::from_slice(data);
    let members = (&mut deserializer).deserialize_map(Members(names))?;
    deserializer.end()?;
    Ok(members)
}

/// An object's members named here, as values; the others are skipped unparsed, as Go's json/v2
/// skips unknown members.
pub(crate) struct Members<'a>(pub(crate) &'a [&'a str]);

impl<'de> Visitor<'de> for Members<'_> {
    type Value = Map<String, Value>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<Self::Value, A::Error> {
        let mut members = Map::new();
        while let Some(name) = entries.next_key::<String>()? {
            if self.0.contains(&name.as_str()) {
                members.insert(name, entries.next_value()?);
            } else {
                entries.next_value::<IgnoredAny>()?;
            }
        }
        Ok(members)
    }
}

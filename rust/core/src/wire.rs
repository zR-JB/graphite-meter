//! Text probe frames and upload progress records shared by server and clients.

use serde::de::{self, Error as _, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};
use std::fmt;

pub const MAX_UPLOAD_COUNTER: u64 = (1 << 53) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    MalformedProbe,
    InvalidUploadProgress,
    UploadCounterOutOfRange,
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MalformedProbe => "malformed ping protocol message",
            Self::InvalidUploadProgress => "invalid upload progress record",
            Self::UploadCounterOutOfRange => "upload progress counter exceeds exact JSON range",
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
    decimal(
        message
            .strip_prefix("PING,")
            .ok_or(WireError::MalformedProbe)?,
        10,
    )
}

pub fn decode_pong(message: &str) -> Result<Pong, WireError> {
    let payload = message
        .strip_prefix("PONG,")
        .ok_or(WireError::MalformedProbe)?;
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

/// Decodes JSON while rejecting duplicate members at every depth, including unknown fields.
pub fn decode_json<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, serde_json::Error> {
    let StrictValue(value) = serde_json::from_slice::<StrictValue>(data)?;
    serde_json::from_value(value)
}

pub fn decode_upload_progress(data: &[u8]) -> Result<UploadProgress, WireError> {
    let StrictValue(Value::Object(mut fields)) = serde_json::from_slice::<StrictValue>(data)
        .map_err(|_| WireError::InvalidUploadProgress)?
    else {
        return Err(WireError::InvalidUploadProgress);
    };
    let kind = match fields.remove("type") {
        Some(Value::String(kind)) => kind,
        _ => return Err(WireError::InvalidUploadProgress),
    };
    let event = match kind.as_str() {
        "ready" => UploadProgress::Ready,
        "error" => {
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
    if !number.is_finite()
        || number < 0.0
        || number > MAX_UPLOAD_COUNTER as f64
        || number.trunc() != number
    {
        return Err(WireError::InvalidUploadProgress);
    }
    Ok(number as u64)
}

/// Deserialize unknown fields too, so duplicate names nested in additive fields
/// cannot bypass the duplicate-field rejection of Go's JSON decoder.
struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object fields")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Number::from_f64(value)
            .map(|number| StrictValue(Value::Number(number)))
            .ok_or_else(|| E::custom("nonfinite JSON number"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.into())))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(StrictValue(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<Self::Value, A::Error> {
        let mut fields = Map::new();
        while let Some((name, StrictValue(value))) = entries.next_entry::<String, StrictValue>()? {
            if fields.insert(name, value).is_some() {
                return Err(A::Error::custom("duplicate JSON field"));
            }
        }
        Ok(StrictValue(Value::Object(fields)))
    }
}

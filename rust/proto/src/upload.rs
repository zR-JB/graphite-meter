//! Upload progress records and receiver counters (`api/upload.md`).

use crate::{json, refusal::UploadRefusal, token};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

/// The largest counter both clients represent exactly.
pub const MAX_COUNTER: u64 = (1 << 53) - 1;

/// Readers bound each record to this many bytes.
pub const MAX_RECORD_BYTES: usize = 64 << 10;

/// A blank line: the feed is alive, with no observation.
pub const HEARTBEAT: &str = "\n";

/// `POST /upload/session`'s answer: the ID of a new upload aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub upload_id: String,
}

impl Session {
    /// Reads the answer, refusing an upload ID that is empty or longer than 8192 bytes.
    pub fn decode(data: &[u8]) -> Result<Self, serde_json::Error> {
        let session: Self = json::decode(data)?;
        match token::valid(&session.upload_id) {
            true => Ok(session),
            false => Err(serde_json::Error::custom("invalid upload ID")),
        }
    }
}

/// One receiver observation: payload bytes and nanoseconds since the first accepted chunk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    #[serde(deserialize_with = "counter")]
    bytes: u64,
    #[serde(deserialize_with = "counter")]
    nanos: u64,
}

impl Counters {
    /// Counters past the exact JSON range stay at its maximum.
    pub fn new(bytes: u64, nanos: u64) -> Self {
        Self { bytes: bytes.min(MAX_COUNTER), nanos: nanos.min(MAX_COUNTER) }
    }

    pub fn bytes(self) -> u64 {
        self.bytes
    }

    pub fn nanos(self) -> u64 {
        self.nanos
    }

    /// Whether this observation may follow `last`; one where either counter regresses is stale.
    pub fn follows(self, last: Self) -> bool {
        self.bytes >= last.bytes && self.nanos >= last.nanos
    }
}

/// A JSON number read as Go and the browser read it, then held to an exact integer counter.
fn counter<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let number = f64::deserialize(deserializer)?;
    if !(0.0..=MAX_COUNTER as f64).contains(&number) || number.fract() != 0.0 {
        return Err(D::Error::custom("invalid upload progress counter"));
    }
    Ok(number as u64)
}

/// One record of a progress feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Record {
    /// The feed is attached; this proves no delivery.
    Ready,
    /// One cumulative receiver observation.
    Progress(Counters),
    /// The final observation, after finalization and lane drain.
    Complete(Counters),
    /// An explicit refusal; an empty field was absent.
    Error {
        #[serde(skip_serializing_if = "String::is_empty")]
        code: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        message: String,
    },
}

impl Record {
    /// The record and its newline.
    pub fn line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("records serialize");
        line.push('\n');
        line
    }

    /// Reads a record without its newline; a heartbeat, malformed or unknown record is no observation.
    pub fn decode(line: &[u8]) -> Result<Self, serde_json::Error> {
        #[derive(Deserialize)]
        struct Type {
            r#type: String,
        }
        #[derive(Deserialize)]
        struct Detail {
            #[serde(default)]
            code: String,
            #[serde(default)]
            message: String,
        }
        let Type { r#type } = json::decode(line)?;
        match r#type.as_str() {
            "ready" => Ok(Self::Ready),
            "progress" => Ok(Self::Progress(serde_json::from_slice(line)?)),
            "complete" => Ok(Self::Complete(serde_json::from_slice(line)?)),
            "error" => {
                let Detail { code, message } = serde_json::from_slice(line)?;
                Ok(Self::Error { code, message })
            }
            _ => Err(serde_json::Error::custom("unknown upload progress record")),
        }
    }
}

impl From<UploadRefusal> for Record {
    fn from(refusal: UploadRefusal) -> Self {
        Self::Error {
            code: refusal.name().into(),
            message: refusal.message().into(),
        }
    }
}

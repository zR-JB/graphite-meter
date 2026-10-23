//! Upload progress records and receiver counters (`api/upload.md`).

use crate::{json, refusal::Refusal, text, token};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

/// The largest counter both clients represent exactly.
pub const MAX_COUNTER: u64 = (1 << 53) - 1;

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
    /// Reads the answer, refusing an upload ID that is not a token.
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
        Self {
            bytes: bytes.min(MAX_COUNTER),
            nanos: nanos.min(MAX_COUNTER),
        }
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

/// Any JSON number spelling of an exact integer counter, as the browser reads it.
fn counter<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let number = f64::deserialize(deserializer)?;
    if !(0.0..=MAX_COUNTER as f64).contains(&number) || number.fract() != 0.0 {
        return Err(D::Error::custom("invalid upload progress counter"));
    }
    Ok(number as u64)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Record {
    /// The feed is attached; this proves no delivery.
    Ready,
    Progress(Counters),
    /// The final observation, after finalization and lane drain.
    Complete(Counters),
    /// An explicit refusal; an empty field was absent, and decoding cleans both.
    Error {
        #[serde(skip_serializing_if = "String::is_empty")]
        message: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        code: String,
    },
}

impl Record {
    pub fn line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("records serialize");
        line.push('\n');
        line
    }

    /// Reads a record without its newline; a heartbeat, malformed or unknown record is no observation.
    pub fn decode(line: &[u8]) -> Result<Self, serde_json::Error> {
        #[derive(Deserialize)]
        struct Sent {
            r#type: String,
            #[serde(default, deserialize_with = "some_counter")]
            bytes: Option<u64>,
            #[serde(default, deserialize_with = "some_counter")]
            nanos: Option<u64>,
            #[serde(default)]
            code: String,
            #[serde(default)]
            message: String,
        }
        let Sent {
            r#type,
            bytes,
            nanos,
            code,
            message,
        } = json::decode(line)?;
        let counters = || match (bytes, nanos) {
            (Some(bytes), Some(nanos)) => Ok(Counters { bytes, nanos }),
            _ => Err(serde_json::Error::custom("missing upload progress counter")),
        };
        match r#type.as_str() {
            "ready" => Ok(Self::Ready),
            "progress" => Ok(Self::Progress(counters()?)),
            "complete" => Ok(Self::Complete(counters()?)),
            "error" => Ok(Self::Error {
                code: text::clean(&code, 64),
                message: text::clean(&message, 256),
            }),
            _ => Err(serde_json::Error::custom("unknown upload progress record")),
        }
    }
}

fn some_counter<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    counter(deserializer).map(Some)
}

impl From<Refusal> for Record {
    fn from(refusal: Refusal) -> Self {
        Self::Error {
            code: refusal.code().into(),
            message: refusal.message().into(),
        }
    }
}

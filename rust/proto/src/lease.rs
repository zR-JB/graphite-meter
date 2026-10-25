//! The lease (`api/discovery.md`): the admission a client takes once, which its measurements then present.

use crate::{json, token};
use http::HeaderName;
use serde::{Deserialize, Serialize, de::Error as _};
use std::time::Duration;

/// Carries the token on HTTP requests; WebSocket and WebTransport URLs, which cannot carry a field, use `lease=`.
pub const LEASE_HEADER: HeaderName = HeaderName::from_static("graphite-meter-lease");
pub const LEASE_PARAMETER: &str = "lease";
/// A lease with nothing live ends after this long.
pub const UNUSED_BOUND: Duration = Duration::from_secs(30);
/// One `POST /lease` mints at most this many upload IDs.
pub const MAX_UPLOADS: u32 = 4;

/// `POST /lease`'s body: the most measurements the client holds at once, of them WebTransport transfer sessions,
/// and how many upload IDs to mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRequest {
    pub measurements: u32,
    pub sessions: u32,
    #[serde(default)]
    pub uploads: u32,
}

impl LeaseRequest {
    /// Refuses a lease of no measurements, more sessions than measurements, or more than `MAX_UPLOADS` IDs.
    pub fn decode(data: &[u8]) -> Result<Self, serde_json::Error> {
        let request: Self = json::decode(data)?;
        let valid = request.measurements > 0
            && request.sessions <= request.measurements
            && request.uploads <= MAX_UPLOADS;
        match valid {
            true => Ok(request),
            false => Err(serde_json::Error::custom("invalid lease request")),
        }
    }
}

/// The answer: the token, what the lease holds, which may be less than asked, and the new upload IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub token: String,
    pub measurements: u32,
    pub sessions: u32,
    pub uploads: Vec<String>,
}

impl Lease {
    pub fn decode(data: &[u8]) -> Result<Self, serde_json::Error> {
        let lease: Self = json::decode(data)?;
        let valid = token::valid(&lease.token)
            && lease.uploads.len() <= MAX_UPLOADS as usize
            && lease.uploads.iter().all(|id| token::valid(id));
        match valid {
            true => Ok(lease),
            false => Err(serde_json::Error::custom("invalid lease")),
        }
    }
}

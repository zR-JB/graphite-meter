//! Says how a server tells its peer why a lane ended (`api/laneendings.txt`).

use crate::refusal::Refusal;
use std::time::Duration;

/// Every lane ends after this long without traffic from its peer.
pub const IDLE_BOUND: Duration = Duration::from_secs(30);
/// Streams a WebTransport lane carries: a download's `streams=` and the uploads read at once (`api/wire.md`).
pub const MAX_LANE_STREAMS: usize = 16;

table! {
    /// Says why a lane ended, with its name, WebSocket close code, WebTransport session code and reason text.
    pub enum LaneEnding {
        name.0: &'static str,
        websocket_code.1: u16,
        webtransport_code.2: u32,
        /// The close reason also serves as the text of an HTTP refusal for the same ending.
        reason.3: &'static str,
    } {
        Finished => ("finished", 1000, 0, ""),
        Idle => ("idle", 4001, 1, "idle"),
        Lifetime => ("lifetime", 4002, 2, "lifetime"),
        Revoked => ("revoked", 1008, 3, "authentication required"),
        Shutdown => ("shutdown", 1001, 4, "shutdown"),
    }
}

impl LaneEnding {
    pub const fn refusal(self) -> Option<Refusal> {
        match self {
            Self::Idle => Some(Refusal::Idle),
            Self::Revoked => Some(Refusal::Revoked),
            Self::Finished | Self::Lifetime | Self::Shutdown => None,
        }
    }
}

//! How a server tells its peer why a lane ended (`api/laneendings.txt`).

use crate::refusal::UploadRefusal;
use std::time::Duration;

/// Every lane ends after this long without traffic from its peer.
pub const IDLE_BOUND: Duration = Duration::from_secs(30);

table! {
    /// Why a lane ended: its name, WebSocket close code, WebTransport session code and reason text.
    pub enum LaneEnding: (&'static str, u16, u32, &'static str) {
        Finished => ("finished", 1000, 0, ""),
        Idle => ("idle", 4001, 1, "idle"),
        Lifetime => ("lifetime", 4002, 2, "lifetime"),
        Revoked => ("revoked", 1008, 3, "authentication required"),
        Shutdown => ("shutdown", 1001, 4, "shutdown"),
    }
}

impl LaneEnding {
    pub const fn name(self) -> &'static str {
        self.row().0
    }

    pub const fn websocket_code(self) -> u16 {
        self.row().1
    }

    pub const fn webtransport_code(self) -> u32 {
        self.row().2
    }

    /// The close reason, also the text of an HTTP refusal for the same ending.
    pub const fn reason(self) -> &'static str {
        self.row().3
    }

    /// The refusal that answers an HTTP upload ending so.
    pub const fn upload_refusal(self) -> Option<UploadRefusal> {
        match self {
            Self::Idle => Some(UploadRefusal::Idle),
            Self::Revoked => Some(UploadRefusal::Revoked),
            Self::Finished | Self::Lifetime | Self::Shutdown => None,
        }
    }

    pub fn from_websocket_code(code: u16) -> Option<Self> {
        Self::ALL.iter().copied().find(|end| end.websocket_code() == code)
    }

    pub fn from_webtransport_code(code: u32) -> Option<Self> {
        Self::ALL.iter().copied().find(|end| end.webtransport_code() == code)
    }
}

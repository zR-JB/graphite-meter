#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureReason {
    PreparationFailed,
    ConnectionLost,
    Timeout,
    SignInRequired,
    ServerBusy,
    ProtocolError,
    InsufficientEvidence,
}

impl FailureReason {
    fn row(self) -> (&'static str, &'static str) {
        include_str!("../../../api/failurereasons.txt")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .nth(self as usize)
            .and_then(|line| line.split_once('|'))
            .map(|(name, label)| (name.trim(), label.trim()))
            .expect("failure vocabulary is pinned")
    }
    pub fn name(self) -> &'static str {
        self.row().0
    }
    pub fn label(self) -> &'static str {
        self.row().1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneEnding {
    Finished,
    Idle,
    Lifetime,
    Revoked,
    Shutdown,
}

impl LaneEnding {
    pub fn name(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Idle => "idle",
            Self::Lifetime => "lifetime",
            Self::Revoked => "revoked",
            Self::Shutdown => "shutdown",
        }
    }
    pub fn websocket_code(self) -> u16 {
        match self {
            Self::Finished => 1000,
            Self::Idle => 4001,
            Self::Lifetime => 4002,
            Self::Revoked => 1008,
            Self::Shutdown => 1001,
        }
    }
    pub fn webtransport_code(self) -> u32 {
        match self {
            Self::Finished => 0,
            Self::Idle => 1,
            Self::Lifetime => 2,
            Self::Revoked => 3,
            Self::Shutdown => 4,
        }
    }
    pub fn reason(self) -> &'static str {
        match self {
            Self::Finished => "",
            Self::Idle => "idle",
            Self::Lifetime => "lifetime",
            Self::Revoked => "authentication required",
            Self::Shutdown => "shutdown",
        }
    }
    pub fn from_websocket_code(code: u16) -> Option<Self> {
        match code {
            1000 => Some(Self::Finished),
            4001 => Some(Self::Idle),
            4002 => Some(Self::Lifetime),
            1008 => Some(Self::Revoked),
            1001 => Some(Self::Shutdown),
            _ => None,
        }
    }
    pub fn from_webtransport_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Finished),
            1 => Some(Self::Idle),
            2 => Some(Self::Lifetime),
            3 => Some(Self::Revoked),
            4 => Some(Self::Shutdown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadRefusal {
    Invalid,
    GlobalFull,
    ClientFull,
    OwnerMismatch,
    Idle,
    Revoked,
}

impl UploadRefusal {
    pub fn name(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::GlobalFull => "globalFull",
            Self::ClientFull => "clientFull",
            Self::OwnerMismatch => "ownerMismatch",
            Self::Idle => "idle",
            Self::Revoked => "revoked",
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::Invalid => "unknown upload id",
            Self::GlobalFull => "upload capacity exhausted",
            Self::ClientFull => "client upload capacity exhausted",
            Self::OwnerMismatch => "upload id belongs to another client",
            Self::Idle => "idle",
            Self::Revoked => "authentication required",
        }
    }
    pub fn status(self) -> u16 {
        match self {
            Self::Invalid => 400,
            Self::GlobalFull => 503,
            Self::ClientFull => 429,
            Self::OwnerMismatch => 403,
            Self::Idle => 408,
            Self::Revoked => 403,
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "invalid" => Some(Self::Invalid),
            "globalFull" => Some(Self::GlobalFull),
            "clientFull" => Some(Self::ClientFull),
            "ownerMismatch" => Some(Self::OwnerMismatch),
            "idle" => Some(Self::Idle),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

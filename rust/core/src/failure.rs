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
    pub const ALL: [Self; 7] = [
        Self::PreparationFailed,
        Self::ConnectionLost,
        Self::Timeout,
        Self::SignInRequired,
        Self::ServerBusy,
        Self::ProtocolError,
        Self::InsufficientEvidence,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::PreparationFailed => "preparation-failed",
            Self::ConnectionLost => "connection-lost",
            Self::Timeout => "timeout",
            Self::SignInRequired => "sign-in-required",
            Self::ServerBusy => "server-busy",
            Self::ProtocolError => "protocol-error",
            Self::InsufficientEvidence => "insufficient-evidence",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::PreparationFailed => "Couldn't prepare the connection",
            Self::ConnectionLost => "Connection lost",
            Self::Timeout => "Stopped delivering data",
            Self::SignInRequired => "Sign-in required",
            Self::ServerBusy => "Server at capacity",
            Self::ProtocolError => "Unexpected server response",
            Self::InsufficientEvidence => "Too little measured time",
        }
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
    pub const ALL: [Self; 5] = [
        Self::Finished,
        Self::Idle,
        Self::Lifetime,
        Self::Revoked,
        Self::Shutdown,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Idle => "idle",
            Self::Lifetime => "lifetime",
            Self::Revoked => "revoked",
            Self::Shutdown => "shutdown",
        }
    }
    pub const fn websocket_code(self) -> u16 {
        match self {
            Self::Finished => 1000,
            Self::Idle => 4001,
            Self::Lifetime => 4002,
            Self::Revoked => 1008,
            Self::Shutdown => 1001,
        }
    }
    pub const fn webtransport_code(self) -> u32 {
        match self {
            Self::Finished => 0,
            Self::Idle => 1,
            Self::Lifetime => 2,
            Self::Revoked => 3,
            Self::Shutdown => 4,
        }
    }
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Finished => "",
            Self::Idle => "idle",
            Self::Lifetime => "lifetime",
            Self::Revoked => "authentication required",
            Self::Shutdown => "shutdown",
        }
    }
    pub fn from_websocket_code(code: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|ending| ending.websocket_code() == code)
    }
    pub fn from_webtransport_code(code: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|ending| ending.webtransport_code() == code)
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
    pub const ALL: [Self; 6] = [
        Self::Invalid,
        Self::GlobalFull,
        Self::ClientFull,
        Self::OwnerMismatch,
        Self::Idle,
        Self::Revoked,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::GlobalFull => "globalFull",
            Self::ClientFull => "clientFull",
            Self::OwnerMismatch => "ownerMismatch",
            Self::Idle => "idle",
            Self::Revoked => "revoked",
        }
    }
    pub const fn message(self) -> &'static str {
        match self {
            Self::Invalid => "unknown upload id",
            Self::GlobalFull => "upload capacity exhausted",
            Self::ClientFull => "client upload capacity exhausted",
            Self::OwnerMismatch => "upload id belongs to another client",
            Self::Idle => "idle",
            Self::Revoked => "authentication required",
        }
    }
    pub const fn status(self) -> u16 {
        match self {
            Self::Invalid => 400,
            Self::GlobalFull => 503,
            Self::ClientFull => 429,
            Self::OwnerMismatch => 403,
            Self::Idle => 408,
            Self::Revoked => 403,
        }
    }
    pub const fn failure_reason(self) -> FailureReason {
        match self {
            Self::Invalid | Self::OwnerMismatch => FailureReason::ProtocolError,
            Self::GlobalFull | Self::ClientFull => FailureReason::ServerBusy,
            Self::Idle => FailureReason::Timeout,
            Self::Revoked => FailureReason::SignInRequired,
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|refusal| refusal.name() == name)
    }
}

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
    const fn row(self) -> (&'static str, &'static str) {
        match self {
            Self::PreparationFailed => ("preparation-failed", "Couldn't prepare the connection"),
            Self::ConnectionLost => ("connection-lost", "Connection lost"),
            Self::Timeout => ("timeout", "Stopped delivering data"),
            Self::SignInRequired => ("sign-in-required", "Sign-in required"),
            Self::ServerBusy => ("server-busy", "Server at capacity"),
            Self::ProtocolError => ("protocol-error", "Unexpected server response"),
            Self::InsufficientEvidence => ("insufficient-evidence", "Too little measured time"),
        }
    }
    pub const fn name(self) -> &'static str {
        self.row().0
    }
    pub const fn label(self) -> &'static str {
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
    pub const ALL: [Self; 5] = [
        Self::Finished,
        Self::Idle,
        Self::Lifetime,
        Self::Revoked,
        Self::Shutdown,
    ];
    const fn row(self) -> (&'static str, u16, u32, &'static str) {
        match self {
            Self::Finished => ("finished", 1000, 0, ""),
            Self::Idle => ("idle", 4001, 1, "idle"),
            Self::Lifetime => ("lifetime", 4002, 2, "lifetime"),
            Self::Revoked => ("revoked", 1008, 3, "authentication required"),
            Self::Shutdown => ("shutdown", 1001, 4, "shutdown"),
        }
    }
    pub const fn name(self) -> &'static str {
        self.row().0
    }
    pub const fn websocket_code(self) -> u16 {
        self.row().1
    }
    pub const fn webtransport_code(self) -> u32 {
        self.row().2
    }
    pub const fn reason(self) -> &'static str {
        self.row().3
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
    const fn row(self) -> (&'static str, &'static str, u16) {
        match self {
            Self::Invalid => ("invalid", "unknown upload id", 400),
            Self::GlobalFull => ("globalFull", "upload capacity exhausted", 503),
            Self::ClientFull => ("clientFull", "client upload capacity exhausted", 429),
            Self::OwnerMismatch => ("ownerMismatch", "upload id belongs to another client", 403),
            Self::Idle => ("idle", "idle", 408),
            Self::Revoked => ("revoked", "authentication required", 403),
        }
    }
    pub const fn name(self) -> &'static str {
        self.row().0
    }
    pub const fn message(self) -> &'static str {
        self.row().1
    }
    pub const fn status(self) -> u16 {
        self.row().2
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

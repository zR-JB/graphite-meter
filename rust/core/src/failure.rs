vocabulary! {
    pub enum FailureReason -> (&'static str, &'static str) {
        PreparationFailed => ("preparation-failed", "Couldn't prepare the connection"),
        ConnectionLost => ("connection-lost", "Connection lost"),
        Timeout => ("timeout", "Stopped delivering data"),
        SignInRequired => ("sign-in-required", "Sign-in required"),
        ServerBusy => ("server-busy", "Server at capacity"),
        ProtocolError => ("protocol-error", "Unexpected server response"),
        InsufficientEvidence => ("insufficient-evidence", "Too little measured time"),
    }
}

impl FailureReason {
    pub const fn name(self) -> &'static str {
        self.row().0
    }
    pub const fn label(self) -> &'static str {
        self.row().1
    }
}

vocabulary! {
    pub enum LaneEnding -> (&'static str, u16, u32, &'static str) {
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

vocabulary! {
    pub enum UploadRefusal -> (&'static str, &'static str, u16, FailureReason) {
        Invalid => ("invalid", "unknown upload id", 400, FailureReason::ProtocolError),
        GlobalFull => ("globalFull", "upload capacity exhausted", 503, FailureReason::ServerBusy),
        ClientFull => ("clientFull", "client upload capacity exhausted", 429, FailureReason::ServerBusy),
        OwnerMismatch => ("ownerMismatch", "upload id belongs to another client", 403, FailureReason::ProtocolError),
        Idle => ("idle", "idle", 408, FailureReason::Timeout),
        Revoked => ("revoked", "authentication required", 403, FailureReason::SignInRequired),
    }
}

impl UploadRefusal {
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
        self.row().3
    }
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|refusal| refusal.name() == name)
    }
}

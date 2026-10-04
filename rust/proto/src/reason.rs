//! Why a server's stage failed (`api/failurereasons.txt`).

table! {
    /// Why a server's stage failed, with the label every view shows.
    pub enum FailureReason: (&'static str, &'static str) {
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
    pub const fn key(self) -> &'static str {
        self.row().0
    }

    pub const fn label(self) -> &'static str {
        self.row().1
    }
}

//! Says why a server's stage failed (`api/failurereasons.txt`).

table! {
    /// Says why a server's stage failed, with the label every view shows.
    pub enum FailureReason {
        key.0: &'static str,
        label.1: &'static str,
    } {
        PathFailed => ("path-failed", "Path unavailable"),
        ConnectionLost => ("connection-lost", "Connection lost"),
        Timeout => ("timeout", "Stopped delivering data"),
        SignInRequired => ("sign-in-required", "Sign-in required"),
        ClientLimit => ("client-limit", "Limit reached for your address or sign-in"),
        ServerCapacity => ("server-capacity", "Server at capacity"),
        ProtocolError => ("protocol-error", "Unexpected server response"),
        InsufficientEvidence => ("insufficient-evidence", "Too little measured time"),
    }
}

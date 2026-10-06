//! Why a server refused an upload (`api/uploadrefusals.txt`, `api/uploadrefusalreasons.txt`).

use crate::reason::FailureReason;

table! {
    /// Why a server refused an upload: its name, exact message, HTTP status and the failure it ends a stage with.
    pub enum UploadRefusal {
        /// The code in `X-Graphite-Upload-Refusal` and in a progress `error` record.
        name.0: &'static str,
        message.1: &'static str,
        status.2: u16,
        failure_reason.3: FailureReason,
    } {
        Invalid => ("invalid", "unknown upload id", 400, FailureReason::ProtocolError),
        GlobalFull => ("globalFull", "upload capacity exhausted", 503, FailureReason::ServerBusy),
        ClientFull => ("clientFull", "client upload capacity exhausted", 429, FailureReason::ServerBusy),
        OwnerMismatch => ("ownerMismatch", "upload id belongs to another client", 403, FailureReason::ProtocolError),
        Idle => ("idle", "idle", 408, FailureReason::Timeout),
        Revoked => ("revoked", "authentication required", 403, FailureReason::SignInRequired),
    }
}

impl UploadRefusal {
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|refusal| refusal.name() == name)
    }
}

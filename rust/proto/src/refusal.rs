//! Why a server refused an upload (`api/uploadrefusals.txt`, `api/uploadrefusalreasons.txt`).

use crate::reason::FailureReason;
use http::StatusCode;

table! {
    /// Why a server refused an upload: its name, exact message, HTTP status and the failure it ends a stage with.
    pub enum UploadRefusal {
        /// The code in `X-Graphite-Upload-Refusal` and in a progress `error` record.
        name.0: &'static str,
        message.1: &'static str,
        status.2: StatusCode,
        failure_reason.3: FailureReason,
    } {
        Invalid => ("invalid", "unknown upload id", StatusCode::BAD_REQUEST, FailureReason::ProtocolError),
        GlobalFull => ("globalFull", "upload capacity exhausted", StatusCode::SERVICE_UNAVAILABLE, FailureReason::ServerBusy),
        ClientFull => ("clientFull", "client upload capacity exhausted", StatusCode::TOO_MANY_REQUESTS, FailureReason::ServerBusy),
        OwnerMismatch => ("ownerMismatch", "upload id belongs to another client", StatusCode::FORBIDDEN, FailureReason::ProtocolError),
        Idle => ("idle", "idle", StatusCode::REQUEST_TIMEOUT, FailureReason::Timeout),
        Revoked => ("revoked", "authentication required", StatusCode::FORBIDDEN, FailureReason::SignInRequired),
    }
}

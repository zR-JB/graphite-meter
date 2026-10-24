//! Why a server refused a request (`api/refusals.txt`).

use crate::reason::FailureReason;
use http::{HeaderName, StatusCode};

pub const REFUSAL_HEADER: HeaderName = HeaderName::from_static("graphite-meter-refusal");
/// Marks, with `required`, a refusal that a sign-in would lift.
pub const SIGN_IN_HEADER: HeaderName = HeaderName::from_static("graphite-meter-auth");

table! {
    /// Why a server refused a request: its code, exact message, HTTP status and the failure it ends a stage with.
    pub enum Refusal {
        /// The code in `Graphite-Meter-Refusal` and in a progress `error` record.
        code.0: &'static str,
        message.1: &'static str,
        status.2: StatusCode,
        failure_reason.3: FailureReason,
    } {
        Invalid => ("invalid", "unknown upload id", StatusCode::BAD_REQUEST, FailureReason::ProtocolError),
        OwnerMismatch => ("owner-mismatch", "upload id belongs to another client", StatusCode::FORBIDDEN, FailureReason::ProtocolError),
        Idle => ("idle", "idle", StatusCode::REQUEST_TIMEOUT, FailureReason::Timeout),
        Revoked => ("revoked", "authentication required", StatusCode::FORBIDDEN, FailureReason::SignInRequired),
        ClientLimit => ("client-limit", "measurement limit for this client reached", StatusCode::TOO_MANY_REQUESTS, FailureReason::ClientLimit),
        ServerCapacity => ("server-capacity", "measurement capacity reached", StatusCode::SERVICE_UNAVAILABLE, FailureReason::ServerCapacity),
        WrongProtocol => ("wrong-protocol", "this port serves transfers over HTTP/3, not TCP", StatusCode::CONFLICT, FailureReason::PathFailed),
    }
}

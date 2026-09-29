use crate::Error;
use graphite_meter_core::failure::{FailureReason, LaneEnding, UploadRefusal};
use std::{fmt, sync::Arc, time::Duration};

/// The client's own failures; `reason`, `permanent` and `retryable` tell them apart by kind.
#[derive(Debug)]
pub enum Failure {
    /// The server's answer: its status, the wait it asked for, and an upload refusal's code.
    Http {
        status: u16,
        retry_after: Duration,
        refusal: Option<UploadRefusal>,
    },
    /// The server asks for sign-in at `origin`. The login page is empty when the server named
    /// none, as for a revoked lane: checking the servers again finds it, as Go's client re-prepares.
    SignIn { origin: String, login_url: String },
    /// Go's ErrApprovalExpired: the approval window closed while the server kept answering.
    ApprovalExpired,
    /// The approval window closed after the last poll failed to reach the server.
    ApprovalUnreachable(Error),
    /// The server ended a lane or a latency channel this way.
    Lane(LaneEnding),
    /// A measurement rule failed the server for this reason.
    Measurement(FailureReason),
    /// A latency channel that closed or failed, which is dialled again.
    Disconnected(&'static str),
    /// What was lost and not replaced within Go's redial window, with the last cause.
    NotReplaced(&'static str, Error),
    /// A lane's failure, which its stage and its retries share.
    Shared(Arc<Error>),
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http {
                status,
                refusal: Some(refusal),
                ..
            } => write!(formatter, "{} (HTTP {status})", refusal.message()),
            Self::Http { status, .. } => write!(formatter, "server returned HTTP {status}"),
            Self::SignIn { login_url, .. } if login_url.is_empty() => formatter.write_str("authentication required"),
            Self::SignIn { login_url, .. } => write!(formatter, "authentication required at {login_url}"),
            Self::ApprovalExpired => formatter.write_str("browser approval timed out"),
            Self::ApprovalUnreachable(error) => {
                write!(
                    formatter,
                    "server unreachable while waiting for browser approval: {error}"
                )
            }
            Self::Lane(ending) => formatter.write_str(ending.reason()),
            Self::Measurement(reason) => formatter.write_str(reason.label()),
            Self::Disconnected(what) => formatter.write_str(what),
            Self::NotReplaced(what, error) => {
                let window = crate::transport::REDIAL_WINDOW;
                write!(formatter, "{what} lost and not replaced within {window:?}: {error}")
            }
            Self::Shared(error) => fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ApprovalUnreachable(error) | Self::NotReplaced(_, error) => Some(error.as_ref()),
            Self::Shared(error) => Some(error.as_ref().as_ref()),
            _ => None,
        }
    }
}

/// Go's statusError.busy (failure.go:44-46).
fn busy(status: u16) -> bool {
    matches!(status, 429 | 503)
}

/// The error and its causes. io::Error::source() skips its own payload, which carries a wrapped cause.
pub(crate) fn causes<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> impl Iterator<Item = &'a (dyn std::error::Error + 'static)> {
    std::iter::successors(Some(error), |error| match error.downcast_ref::<std::io::Error>() {
        Some(io) => io.get_ref().map(|inner| inner as &(dyn std::error::Error + 'static)),
        None => error.source(),
    })
}

/// The sign-in the server asked for behind `error`: its origin and login page.
pub(crate) fn sign_in<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<(&'a str, &'a str)> {
    causes(error).find_map(|cause| match cause.downcast_ref() {
        Some(Failure::SignIn { origin, login_url }) => Some((origin.as_str(), login_url.as_str())),
        _ => None,
    })
}

/// The wait a busy answer behind `error` asked for, if the server was busy.
pub(crate) fn busy_wait(error: &(dyn std::error::Error + 'static)) -> Option<Duration> {
    causes(error).find_map(|cause| match cause.downcast_ref() {
        Some(&Failure::Http {
            status, retry_after, ..
        }) if busy(status) => Some(retry_after),
        _ => None,
    })
}

/// Whether the server's answer behind the error refused the upload id with `refusal`.
pub(crate) fn refused(error: &(dyn std::error::Error + 'static), refusal: UploadRefusal) -> bool {
    causes(error).any(
        |cause| matches!(cause.downcast_ref(), Some(Failure::Http { refusal: Some(found), .. }) if *found == refusal),
    )
}

/// Go's permanent (transfer.go:65-68): a sign-in, or a refusal no retry answers, which
/// uploadRefusal makes of `invalid` and `ownerMismatch` (failure.go:56-60).
pub(crate) fn permanent(error: &(dyn std::error::Error + 'static)) -> bool {
    sign_in(error).is_some() || refused(error, UploadRefusal::Invalid) || refused(error, UploadRefusal::OwnerMismatch)
}

/// A lane retries what Go's persist retries (transfer.go:84-86): all but a permanent error, its
/// own answer other than busy, which laneRefusal makes a refusal (failure.go:48-54), and an HTTP/3
/// or QUIC violation this side found in what the peer sent.
pub(crate) fn retryable(error: &Error) -> bool {
    !permanent(error.as_ref())
        && !matches!(error.downcast_ref(), Some(&Failure::Http { status, .. }) if !busy(status))
        && !crate::quic::violation(error.as_ref())
}

pub fn reason(error: &(dyn std::error::Error + 'static), preparing: bool) -> FailureReason {
    // A proxy setting the client cannot use lost no connection.
    if causes(error).any(|cause| cause.is::<graphite_meter_net::UnusableProxy>()) {
        return FailureReason::PreparationFailed;
    }
    for error in causes(error) {
        match error.downcast_ref() {
            Some(Failure::Measurement(reason)) => return *reason,
            Some(Failure::SignIn { .. } | Failure::Lane(LaneEnding::Revoked)) => return FailureReason::SignInRequired,
            Some(Failure::Http {
                refusal: Some(refusal), ..
            }) => return refusal.failure_reason(),
            Some(&Failure::Http { status, .. }) if busy(status) => return FailureReason::ServerBusy,
            Some(Failure::Http { .. }) => return FailureReason::ProtocolError,
            Some(Failure::Lane(LaneEnding::Shutdown | LaneEnding::Finished)) => return FailureReason::ConnectionLost,
            Some(Failure::Lane(_)) => return FailureReason::Timeout,
            _ => {}
        }
        if error.is::<tokio::time::error::Elapsed>() {
            return FailureReason::Timeout;
        }
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::TimedOut => return FailureReason::Timeout,
                std::io::ErrorKind::InvalidData => {}
                _ => return FailureReason::ConnectionLost,
            }
        }
        if error.is::<serde_json::Error>() || error.is::<graphite_meter_core::wire::WireError>() {
            return FailureReason::ProtocolError;
        }
    }
    if preparing {
        FailureReason::PreparationFailed
    } else {
        FailureReason::ConnectionLost
    }
}

pub fn text(error: &(dyn std::error::Error + 'static)) -> String {
    let mut network = false;
    for cause in causes(error) {
        if cause.is::<graphite_meter_net::Unreachable>() {
            return "Server could not be reached".into();
        }
        if let Some(proxy) = cause.downcast_ref::<graphite_meter_net::UnusableProxy>() {
            return clean(&proxy.to_string(), 320);
        }
        if let Some(rustls::Error::InvalidCertificate(certificate)) = cause.downcast_ref() {
            use rustls::CertificateError::{Expired, ExpiredContext, NotValidYet, NotValidYetContext, UnknownIssuer};
            let detail = match certificate {
                UnknownIssuer => "certificate signed by unknown authority".into(),
                Expired | ExpiredContext { .. } | NotValidYet | NotValidYetContext { .. } => {
                    "certificate has expired or is not yet valid".into()
                }
                // Its Display is the Debug form, so name the wrapped cause.
                rustls::CertificateError::Other(other) => clean(&other.0.to_string(), 200),
                other => clean(&other.to_string(), 200),
            };
            return format!(
                "Certificate not trusted: {detail}. Turn on Skip TLS verify (-insecure) only for a server you trust."
            );
        }
        network |=
            cause.is::<std::io::Error>() || cause.is::<hyper::Error>() || cause.is::<tokio::time::error::Elapsed>();
    }
    if network {
        reason(error, false).label().into()
    } else {
        clean(&error.to_string(), 320)
    }
}

fn clean(text: &str, limit: usize) -> String {
    graphite_meter_core::text::clean_with(text, limit, graphite_meter_core::text::terminal_character)
}

pub(crate) fn lane_error(error: Error) -> Error {
    if let Some(quinn::ConnectionError::ApplicationClosed(close)) = error.downcast_ref::<quinn::ConnectionError>()
        && let Ok(code) = u32::try_from(close.error_code.into_inner())
        && let Some(ending) = LaneEnding::from_webtransport_code(code)
    {
        return Box::new(Failure::Lane(ending));
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphite_meter_core::wire::WireError;
    use std::io::{Error as IoError, ErrorKind};

    #[test]
    fn invalid_data_is_classified_by_its_payload() {
        let wrapped = IoError::new(ErrorKind::InvalidData, WireError::InvalidReceiverCheckpoint);
        assert_eq!(reason(&wrapped, false), FailureReason::ProtocolError);
        let plain = IoError::new(ErrorKind::InvalidData, "undecodable record");
        assert_eq!(reason(&plain, true), FailureReason::PreparationFailed);
        assert_eq!(reason(&plain, false), FailureReason::ConnectionLost);
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_server_reads_as_unreachable() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let socket = tokio::net::TcpSocket::new_v4()?;
        socket.bind("127.0.0.1:0".parse()?)?;
        // A full accept queue drops further handshakes, as a blackholed server does.
        let listener = socket.listen(0)?;
        let address = listener.local_addr()?;
        let _queued = tokio::net::TcpStream::connect(address).await?;
        let http = crate::net::Http::new(false)?;
        let error = http
            .discover(&format!("http://{address}"))
            .await
            .err()
            .ok_or("discovery succeeded")?;
        assert_eq!(text(error.as_ref()), "Server could not be reached");
        Ok(())
    }
}

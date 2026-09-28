use crate::Error;
use graphite_meter_core::failure::{FailureReason, LaneEnding, UploadRefusal};

#[derive(Debug)]
pub struct HttpFailure {
    pub status: u16,
    pub retry_after: std::time::Duration,
    pub refusal: Option<UploadRefusal>,
}

impl HttpFailure {
    pub fn retryable(&self) -> bool {
        matches!(self.status, 429 | 503) || self.status == 408 && self.refusal == Some(UploadRefusal::Idle)
    }
    pub fn reason(&self) -> FailureReason {
        match self.refusal {
            Some(refusal) => refusal.failure_reason(),
            None if matches!(self.status, 429 | 503) => FailureReason::ServerBusy,
            None => FailureReason::ProtocolError,
        }
    }
}

impl std::fmt::Display for HttpFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.refusal {
            Some(refusal) => write!(formatter, "{} (HTTP {})", refusal.message(), self.status),
            None => write!(formatter, "server returned HTTP {}", self.status),
        }
    }
}
impl std::error::Error for HttpFailure {}

#[derive(Debug)]
pub struct LaneFailure(pub LaneEnding);
impl std::fmt::Display for LaneFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0.reason())
    }
}
impl std::error::Error for LaneFailure {}

/// The error and its causes. io::Error::source() skips its own payload, which carries a wrapped cause.
fn causes<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> impl Iterator<Item = &'a (dyn std::error::Error + 'static)> {
    std::iter::successors(Some(error), |error| match error.downcast_ref::<std::io::Error>() {
        Some(io) => io.get_ref().map(|inner| inner as &(dyn std::error::Error + 'static)),
        None => error.source(),
    })
}

/// A lane retries what Go's persist retries (transfer.go:65-68, 84-86): all but a sign-in and an
/// answer other than busy, which laneRefusal makes a refusal (failure.go:48-54), and an HTTP/3 or
/// QUIC violation this side found in what the peer sent.
pub(crate) fn retryable(error: &Error) -> bool {
    crate::net::authentication_required(error.as_ref()).is_none()
        && error.downcast_ref::<HttpFailure>().is_none_or(HttpFailure::retryable)
        && !crate::quic::violation(error.as_ref())
}

pub fn reason(error: &(dyn std::error::Error + 'static), preparing: bool) -> FailureReason {
    // A proxy setting the client cannot use lost no connection.
    if causes(error).any(|cause| cause.is::<graphite_meter_net::UnusableProxy>()) {
        return FailureReason::PreparationFailed;
    }
    for error in causes(error) {
        if let Some(failure) = error.downcast_ref::<MeasurementFailure>() {
            return failure.0;
        }
        if error.is::<crate::net::AuthRequired>() {
            return FailureReason::SignInRequired;
        }
        if let Some(http) = error.downcast_ref::<HttpFailure>() {
            return http.reason();
        }
        if let Some(lane) = error.downcast_ref::<LaneFailure>() {
            return match lane.0 {
                LaneEnding::Revoked => FailureReason::SignInRequired,
                LaneEnding::Shutdown | LaneEnding::Finished => FailureReason::ConnectionLost,
                _ => FailureReason::Timeout,
            };
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
    let text = text.chars().map(|character| {
        if graphite_meter_core::text::terminal_character(character) {
            character
        } else {
            ' '
        }
    });
    let text: String = text.collect();
    if text.chars().count() <= limit {
        return text;
    }
    text.chars().take(limit.saturating_sub(1)).chain(['…']).collect()
}

pub(crate) fn lane_error(error: Error) -> Error {
    if let Some(quinn::ConnectionError::ApplicationClosed(close)) = error.downcast_ref::<quinn::ConnectionError>()
        && let Ok(code) = u32::try_from(close.error_code.into_inner())
        && let Some(ending) = LaneEnding::from_webtransport_code(code)
    {
        return Box::new(LaneFailure(ending));
    }
    error
}

pub(crate) struct SharedFailure(pub std::sync::Arc<Error>);
impl std::fmt::Debug for SharedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.0.as_ref(), f)
    }
}
impl std::fmt::Display for SharedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.0.as_ref(), f)
    }
}
impl std::error::Error for SharedFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}

#[derive(Debug)]
pub struct MeasurementFailure(pub FailureReason);
impl std::fmt::Display for MeasurementFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0.label())
    }
}
impl std::error::Error for MeasurementFailure {}

#[derive(Debug)]
pub(crate) struct NotReplaced(pub &'static str, pub Option<Error>);
impl std::fmt::Display for NotReplaced {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} lost and not replaced within 2s", self.0)?;
        match &self.1 {
            Some(cause) => write!(formatter, ": {cause}"),
            None => Ok(()),
        }
    }
}
impl std::error::Error for NotReplaced {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.1
            .as_deref()
            .map(|cause| cause as &(dyn std::error::Error + 'static))
    }
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

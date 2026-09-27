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

pub fn reason(mut error: &(dyn std::error::Error + 'static), preparing: bool) -> FailureReason {
    loop {
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
        let io = error.downcast_ref::<std::io::Error>();
        if let Some(io) = io {
            match io.kind() {
                std::io::ErrorKind::TimedOut => return FailureReason::Timeout,
                std::io::ErrorKind::InvalidData => {}
                _ => return FailureReason::ConnectionLost,
            }
        }
        if error.is::<serde_json::Error>() || error.is::<graphite_meter_core::wire::WireError>() {
            return FailureReason::ProtocolError;
        }
        // io::Error::source() skips its own payload, which carries a wrapped wire cause.
        let next = match io {
            Some(io) => io.get_ref().map(|inner| inner as &(dyn std::error::Error + 'static)),
            None => error.source(),
        };
        let Some(source) = next else {
            return if preparing {
                FailureReason::PreparationFailed
            } else {
                FailureReason::ConnectionLost
            };
        };
        error = source;
    }
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
}

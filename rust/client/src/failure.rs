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
        matches!(self.status, 429 | 503)
            || self.status == 408 && self.refusal == Some(UploadRefusal::Idle)
    }
    pub fn reason(&self) -> FailureReason {
        match self.refusal {
            Some(UploadRefusal::Invalid) => FailureReason::ConnectionLost,
            Some(UploadRefusal::Revoked) => FailureReason::SignInRequired,
            _ if matches!(self.status, 429 | 503) => FailureReason::ServerBusy,
            Some(UploadRefusal::Idle) => FailureReason::Timeout,
            _ => FailureReason::ProtocolError,
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

pub fn reason(mut error: &(dyn std::error::Error + 'static)) -> FailureReason {
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
        if error.is::<tokio::time::error::Elapsed>()
            || error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::TimedOut)
        {
            return FailureReason::Timeout;
        }
        let Some(source) = error.source() else {
            return FailureReason::ConnectionLost;
        };
        error = source;
    }
}

pub(crate) fn lane_error(error: Error) -> Error {
    if let Some(quinn::ConnectionError::ApplicationClosed(close)) =
        error.downcast_ref::<quinn::ConnectionError>()
        && let Ok(code) = u32::try_from(close.error_code.into_inner())
        && let Some(ending) = LaneEnding::from_webtransport_code(code)
    {
        return Box::new(LaneFailure(ending));
    }
    error
}

#[derive(Debug)]
pub struct MeasurementFailure(pub FailureReason);
impl std::fmt::Display for MeasurementFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0.label())
    }
}
impl std::error::Error for MeasurementFailure {}

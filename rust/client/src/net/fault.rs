//! Failures typed where they arise: their reason, how a retry treats them, and the text a view shows.
use crate::{model::Failure, text::capitalized};
use graphite_meter_net::ConnectError;
use graphite_meter_proto::{
    lane::LaneEnding,
    origin::Origin,
    reason::FailureReason,
    refusal::{SIGN_IN_HEADER, UPLOAD_REFUSAL_HEADER, UploadRefusal},
    route::Route,
    text,
};
use http::{HeaderMap, StatusCode, header};
use rustls::CertificateError;
use std::{fmt, time::Duration};

/// The most characters of a peer's or a library's text a fault shows.
const DETAIL_CHARS: usize = 320;

/// Why a request, a connection or a lane failed.
#[derive(Debug)]
pub enum Fault {
    /// No connection opened.
    Connect(ConnectError),
    /// An open connection or stream broke.
    Lost,
    /// What did not arrive in time.
    TimedOut(&'static str),
    /// The server answered `from` with another status than 200.
    Status {
        status: StatusCode,
        from: Route,
        retry_after: Option<Duration>,
    },
    /// The server refused an upload ID for good.
    Refused(UploadRefusal),
    /// The server ended the lane.
    Ended(LaneEnding),
    /// The server at this origin asks for sign-in.
    SignIn(Origin),
    /// The peer broke the protocol or answered what this client refuses.
    Malformed(String),
}

/// How a retry treats a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Redial,
    /// The server is busy; it asked to wait this long.
    Busy(Duration),
    Final,
}

impl Fault {
    pub fn reason(&self) -> FailureReason {
        match self {
            Self::Connect(ConnectError::Proxy(_) | ConnectError::Tls(_)) => FailureReason::PreparationFailed,
            Self::Connect(_) | Self::Lost | Self::Ended(LaneEnding::Finished | LaneEnding::Shutdown) => {
                FailureReason::ConnectionLost
            }
            Self::SignIn(_) | Self::Ended(LaneEnding::Revoked) => FailureReason::SignInRequired,
            Self::TimedOut(_) | Self::Ended(_) => FailureReason::Timeout,
            Self::Status { status, .. } if busy(*status) => FailureReason::ServerBusy,
            Self::Status { .. } | Self::Malformed(_) => FailureReason::ProtocolError,
            Self::Refused(refusal) => refusal.failure_reason(),
        }
    }

    pub fn class(&self) -> Class {
        match self {
            Self::Status { status, retry_after, .. } if busy(*status) => Class::Busy(retry_after.unwrap_or_default()),
            // An upload's control requests outlast a proxy's passing server error; lanes do not.
            Self::Status { status, from, .. } if status.is_server_error() && upload_control(*from) => Class::Redial,
            Self::Connect(ConnectError::Proxy(_) | ConnectError::Tls(_))
            | Self::Status { .. }
            | Self::Refused(_)
            | Self::SignIn(_)
            | Self::Ended(LaneEnding::Revoked)
            | Self::Malformed(_) => Class::Final,
            Self::Connect(_) | Self::Lost | Self::TimedOut(_) | Self::Ended(_) => Class::Redial,
        }
    }

    pub fn failure(&self) -> Failure {
        Failure::new(self.reason(), self.to_string())
    }

    /// The fault an answer of `status` to `from` means, if any; a sign-in is for the server `issuer` names.
    pub(super) fn answer(
        status: StatusCode,
        headers: &HeaderMap,
        from: Route,
        issuer: impl FnOnce() -> Origin,
    ) -> Option<Self> {
        let header = |name| headers.get(name).and_then(|value| value.to_str().ok());
        if status == StatusCode::OK {
            return None;
        }
        if status == StatusCode::FORBIDDEN && header(SIGN_IN_HEADER) == Some("required") {
            return Some(Self::SignIn(issuer()));
        }
        let seconds = header(header::RETRY_AFTER).filter(|value| value.bytes().all(|byte| byte.is_ascii_digit()));
        let retry_after = seconds
            .and_then(|value| value.parse().ok())
            .map(|seconds: u32| Duration::from_secs(seconds.into()));
        Some(
            match header(UPLOAD_REFUSAL_HEADER).and_then(|name| UploadRefusal::find_by(UploadRefusal::name, name)) {
                Some(refusal) => Self::refusal(refusal, from, retry_after, issuer),
                None => Self::Status { status, from, retry_after },
            },
        )
    }

    /// The fault an upload refusal in an answer to `from` or a progress record means; sign-in is for `issuer`'s server.
    pub(super) fn refusal(
        refusal: UploadRefusal,
        from: Route,
        retry_after: Option<Duration>,
        issuer: impl FnOnce() -> Origin,
    ) -> Self {
        let busy = |status| Self::Status { status, from, retry_after };
        match refusal {
            UploadRefusal::Idle => Self::Ended(LaneEnding::Idle),
            UploadRefusal::Revoked => Self::SignIn(issuer()),
            UploadRefusal::GlobalFull => busy(StatusCode::SERVICE_UNAVAILABLE),
            UploadRefusal::ClientFull => busy(StatusCode::TOO_MANY_REQUESTS),
            refusal => Self::Refused(refusal),
        }
    }
}

impl From<ConnectError> for Fault {
    fn from(error: ConnectError) -> Self {
        Self::Connect(error)
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(ConnectError::Tls(rustls::Error::InvalidCertificate(certificate))) => write!(
                formatter,
                "Certificate not trusted: {}. Turn on Skip TLS verification (--insecure) only for a server you trust.",
                untrusted(certificate)
            ),
            Self::Connect(ConnectError::Unreachable(_)) => formatter.write_str(super::UNREACHABLE),
            Self::Connect(ConnectError::Proxy(unusable)) => {
                formatter.write_str(&text::clean(&unusable.to_string(), DETAIL_CHARS))
            }
            Self::Connect(other) => {
                formatter.write_str(&text::clean(&format!("Could not connect: {other}"), DETAIL_CHARS))
            }
            Self::Status { status, from, .. } => write!(formatter, "HTTP {} from {}", status.as_u16(), source(*from)),
            Self::Refused(refusal) => write!(formatter, "{} (HTTP {})", refusal.message(), refusal.status().as_u16()),
            Self::Ended(ending) => write!(formatter, "The server ended the stream: {}", ending.name()),
            Self::SignIn(_) => formatter.write_str(self.reason().label()),
            Self::Malformed(detail) => {
                formatter.write_str(&text::clean(&format!("Unexpected server response: {detail}"), DETAIL_CHARS))
            }
            Self::TimedOut(what) => write!(formatter, "{} timed out", capitalized(what)),
            Self::Lost => formatter.write_str(self.reason().label()),
        }
    }
}

impl std::error::Error for Fault {}

fn upload_control(route: Route) -> bool {
    matches!(route, Route::UploadSession | Route::UploadProgress | Route::UploadCheckpoint)
}

fn busy(status: StatusCode) -> bool {
    matches!(status, StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE)
}

/// What a status text calls the request: a control request by name, any other by its path.
fn source(route: Route) -> &'static str {
    match route {
        Route::Servers => "the test server list",
        Route::Preflight => "preflight",
        Route::Probe => "probe",
        Route::UploadCheckpoint => "receiver checkpoint",
        Route::UploadSession => "upload session",
        Route::UploadProgress => "upload progress",
        route => route.path(),
    }
}

fn untrusted(certificate: &CertificateError) -> String {
    match certificate {
        CertificateError::UnknownIssuer => "its issuer is unknown".into(),
        CertificateError::Expired
        | CertificateError::ExpiredContext { .. }
        | CertificateError::NotValidYet
        | CertificateError::NotValidYetContext { .. } => "it has expired or is not valid yet".into(),
        CertificateError::Other(other) => text::clean(&other.0.to_string(), 200),
        other => text::clean(&other.to_string(), 200),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;
    use std::assert_matches;

    fn answer(status: u16, headers: &[(&'static str, &'static str)]) -> Option<Fault> {
        let headers = headers
            .iter()
            .map(|(name, value)| (http::HeaderName::from_static(name), HeaderValue::from_static(value)))
            .collect();
        let issuer = || Origin::parse("https://meter.example").unwrap();
        Fault::answer(StatusCode::from_u16(status).unwrap(), &headers, Route::UploadSession, issuer)
    }

    #[test]
    fn answers_map_to_faults_with_their_reason_class_and_text() {
        use FailureReason::*;
        let busy = |seconds| Class::Busy(Duration::from_secs(seconds));
        const REFUSAL: &str = "x-graphite-upload-refusal";
        type Row = (u16, &'static [(&'static str, &'static str)], FailureReason, Class, &'static str);
        const DATE: [(&str, &str); 1] = [("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")];
        const AUTH: [(&str, &str); 1] = [("graphite-meter-auth", "required")];
        const FULL: [(&str, &str); 2] = [(REFUSAL, "clientFull"), ("retry-after", "1")];
        let mismatch = "upload id belongs to another client (HTTP 403)";
        let rows: &[Row] = &[
            (503, &[], ServerBusy, busy(0), "HTTP 503 from upload session"),
            (429, &[("retry-after", "3")], ServerBusy, busy(3), "HTTP 429 from upload session"),
            (429, &DATE, ServerBusy, busy(0), "HTTP 429 from upload session"),
            (500, &[], ProtocolError, Class::Redial, "HTTP 500 from upload session"),
            (204, &[], ProtocolError, Class::Final, "HTTP 204 from upload session"),
            (403, &[], ProtocolError, Class::Final, "HTTP 403 from upload session"),
            (403, &AUTH, SignInRequired, Class::Final, "Sign-in required"),
            (403, &[(REFUSAL, "revoked")], SignInRequired, Class::Final, "Sign-in required"),
            (408, &[(REFUSAL, "idle")], Timeout, Class::Redial, "The server ended the stream: idle"),
            (503, &[(REFUSAL, "globalFull")], ServerBusy, busy(0), "HTTP 503 from upload session"),
            (400, &FULL, ServerBusy, busy(1), "HTTP 429 from upload session"),
            (400, &[(REFUSAL, "invalid")], ProtocolError, Class::Final, "unknown upload id (HTTP 400)"),
            (403, &[(REFUSAL, "ownerMismatch")], ProtocolError, Class::Final, mismatch),
        ];
        for (status, headers, reason, class, text) in rows {
            let fault = answer(*status, headers).unwrap();
            assert_eq!(
                (fault.reason(), fault.class(), fault.to_string().as_str()),
                (*reason, *class, *text),
                "{fault:?}"
            );
        }
        assert!(answer(200, &[(REFUSAL, "invalid")]).is_none());
        let lane = Fault::answer(StatusCode::BAD_GATEWAY, &HeaderMap::new(), Route::Upload, || unreachable!());
        assert_eq!(lane.unwrap().class(), Class::Final, "a lane's server error stands");
        let issuer = answer(403, &AUTH);
        assert_matches!(issuer, Some(Fault::SignIn(origin)) if origin.to_string() == "https://meter.example");
    }

    #[test]
    fn timeouts_and_connections_never_opened_say_what_happened() {
        let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");
        let unreachable = std::io::Error::new(std::io::ErrorKind::TimedOut, "TLS handshake timed out");
        for (fault, text) in [
            (Fault::TimedOut("receiver checkpoint"), "Receiver checkpoint timed out"),
            (Fault::Connect(ConnectError::Io(refused)), "Could not connect: connection refused"),
            (
                Fault::Connect(ConnectError::Refused("proxy said no".into())),
                "Could not connect: proxy said no",
            ),
            (Fault::Connect(ConnectError::Unreachable(unreachable)), super::super::UNREACHABLE),
            (Fault::Lost, "Connection lost"),
        ] {
            assert_eq!(fault.to_string(), text, "{fault:?}");
        }
    }

    #[test]
    fn endings_redial_except_a_revoked_grant() {
        for (ending, reason, class) in [
            (LaneEnding::Idle, FailureReason::Timeout, Class::Redial),
            (LaneEnding::Lifetime, FailureReason::Timeout, Class::Redial),
            (LaneEnding::Shutdown, FailureReason::ConnectionLost, Class::Redial),
            (LaneEnding::Finished, FailureReason::ConnectionLost, Class::Redial),
            (LaneEnding::Revoked, FailureReason::SignInRequired, Class::Final),
        ] {
            let fault = Fault::Ended(ending);
            assert_eq!((fault.reason(), fault.class()), (reason, class), "{ending:?}");
        }
    }
}

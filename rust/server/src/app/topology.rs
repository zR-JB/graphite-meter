//! What each endpoint mounts (`api/routes.txt`, `docs/DEPLOYMENT.md#native-listeners`).

use crate::config::ListenerKind;
use graphite_meter_proto::route::Route;

/// What accepted a connection: a configured TCP listener, or the HTTP/3 address's TCP companion or QUIC endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endpoint {
    H1,
    H1Tls,
    H2,
    H3Companion,
    Quic,
}

/// What a route's work holds while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Unmetered,
    /// A measurement handler.
    Operation,
    /// A WebTransport transfer session and its handler.
    Session,
}

impl Endpoint {
    pub const ALL: [Self; 5] = [Self::H1, Self::H1Tls, Self::H2, Self::H3Companion, Self::Quic];

    /// Whether this endpoint serves `route`: the only mount decision.
    pub const fn mounts(self, route: Route) -> bool {
        match route {
            Route::Probe
            | Route::UploadSession
            | Route::UploadCheckpoint
            | Route::UploadProgress
            | Route::WtSession => true,
            Route::Preflight | Route::Servers | Route::WsSession | Route::Ping => self.ui(),
            Route::Download | Route::Upload => !matches!(self, Self::H3Companion),
            Route::WtDownload | Route::WtUpload | Route::WtPing => matches!(self, Self::Quic),
        }
    }

    /// It serves the browser app and the authentication pages, which answer every path no route claims.
    pub const fn ui(self) -> bool {
        matches!(self, Self::H1 | Self::H1Tls)
    }

    /// Its probe answers point at the HTTP/3 port.
    pub const fn bootstrap(self) -> bool {
        matches!(self, Self::H3Companion)
    }

    /// What its startup line says it serves.
    pub const fn role(self, auth: bool) -> &'static str {
        match self {
            Self::H1 if auth => {
                "HTTP/1.1 clear: trusted proxy upstream only; direct requests are refused, GET / redirects to HTTPS"
            }
            Self::H1 => "HTTP/1.1 clear: UI, discovery, probe, transfers, WebSockets",
            Self::H1Tls => "HTTPS/WSS HTTP/1.1: UI, discovery, probe, transfers, WebSockets",
            Self::H2 => "HTTPS HTTP/2: measurement probe, transfers, progress only",
            Self::H3Companion => "HTTPS HTTP/1.1 companion: HTTP/3 bootstrap probe, upload and ticket control",
            Self::Quic => "HTTP/3: probe, transfers, progress, WebTransport",
        }
    }

    /// The listener setting it belongs to.
    pub const fn listener(self) -> ListenerKind {
        match self {
            Self::H1 => ListenerKind::H1,
            Self::H1Tls => ListenerKind::H1Tls,
            Self::H2 => ListenerKind::H2,
            Self::H3Companion | Self::Quic => ListenerKind::H3,
        }
    }
}

pub const fn admission(route: Route) -> Admission {
    match route {
        Route::Download | Route::Upload | Route::UploadProgress | Route::Ping | Route::WtPing => Admission::Operation,
        Route::WtDownload | Route::WtUpload => Admission::Session,
        Route::Preflight
        | Route::Probe
        | Route::Servers
        | Route::UploadSession
        | Route::UploadCheckpoint
        | Route::WtSession
        | Route::WsSession => Admission::Unmetered,
    }
}

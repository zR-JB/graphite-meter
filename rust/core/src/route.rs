//! Fixed measurement route identities shared by clients and servers.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Http,
    WebSocket,
    WebTransport,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::WebSocket => "ws",
            Self::WebTransport => "wt",
        }
    }
}

/// The operation budget that admits a route; WebTransport ping uses the request budget, not the session budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Unmetered,
    Request,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Preflight,
    Probe,
    Download,
    Upload,
    UploadSession,
    UploadProgress,
    WtSession,
    WsSession,
    Ping,
    WtDownload,
    WtUpload,
    WtPing,
    Servers,
    UploadCheckpoint,
}

pub const ALL: [Route; 14] = [
    Route::Preflight,
    Route::Probe,
    Route::Download,
    Route::Upload,
    Route::UploadSession,
    Route::UploadProgress,
    Route::WtSession,
    Route::WsSession,
    Route::Ping,
    Route::WtDownload,
    Route::WtUpload,
    Route::WtPing,
    Route::Servers,
    Route::UploadCheckpoint,
];

impl Route {
    const fn row(self) -> (&'static str, &'static str, Kind, Admission, &'static [&'static str]) {
        use {Admission::*, Kind::*};
        match self {
            Self::Preflight => ("preflight", "/preflight", Http, Unmetered, &["GET"]),
            Self::Probe => ("probe", "/probe", Http, Unmetered, &["GET"]),
            Self::Download => ("download", "/download", Http, Request, &["GET"]),
            Self::Upload => ("upload", "/upload", Http, Request, &["POST"]),
            Self::UploadSession => ("uploadSession", "/upload/session", Http, Unmetered, &["POST"]),
            Self::UploadProgress => ("uploadProgress", "/upload/progress", Http, Request, &["GET", "DELETE"]),
            Self::WtSession => ("wtSession", "/wt/session", Http, Unmetered, &["POST"]),
            Self::WsSession => ("wsSession", "/ws/session", Http, Unmetered, &["POST"]),
            Self::Ping => ("ping", "/ws/ping", WebSocket, Request, &["GET"]),
            Self::WtDownload => ("wtDownload", "/wt/download", WebTransport, Session, &["CONNECT"]),
            Self::WtUpload => ("wtUpload", "/wt/upload", WebTransport, Session, &["CONNECT"]),
            Self::WtPing => ("wtPing", "/wt/ping", WebTransport, Request, &["CONNECT"]),
            Self::Servers => ("servers", "/servers", Http, Unmetered, &["GET"]),
            Self::UploadCheckpoint => ("uploadCheckpoint", "/upload/checkpoint", Http, Unmetered, &["POST"]),
        }
    }

    pub const fn name(self) -> &'static str {
        self.row().0
    }

    pub const fn path(self) -> &'static str {
        self.row().1
    }

    pub const fn kind(self) -> Kind {
        self.row().2
    }

    pub const fn admission(self) -> Admission {
        self.row().3
    }

    /// The dispatched methods; HEAD and OPTIONS are the mux's, never a CORS grant.
    pub const fn methods(self) -> &'static [&'static str] {
        self.row().4
    }
}

/// Matches the entire path, without URL decoding or slash normalization.
pub fn lookup(path: &str) -> Option<Route> {
    ALL.into_iter().find(|route| route.path() == path)
}

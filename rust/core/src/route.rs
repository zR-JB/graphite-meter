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

vocabulary! {
    pub enum Route -> (&'static str, &'static str, Kind, Admission, &'static [&'static str]) {
        Preflight => ("preflight", "/preflight", Kind::Http, Admission::Unmetered, &["GET"]),
        Probe => ("probe", "/probe", Kind::Http, Admission::Unmetered, &["GET"]),
        Download => ("download", "/download", Kind::Http, Admission::Request, &["GET"]),
        Upload => ("upload", "/upload", Kind::Http, Admission::Request, &["POST"]),
        UploadSession => ("uploadSession", "/upload/session", Kind::Http, Admission::Unmetered, &["POST"]),
        UploadProgress => ("uploadProgress", "/upload/progress", Kind::Http, Admission::Request, &["GET", "DELETE"]),
        WtSession => ("wtSession", "/wt/session", Kind::Http, Admission::Unmetered, &["POST"]),
        WsSession => ("wsSession", "/ws/session", Kind::Http, Admission::Unmetered, &["POST"]),
        Ping => ("ping", "/ws/ping", Kind::WebSocket, Admission::Request, &["GET"]),
        WtDownload => ("wtDownload", "/wt/download", Kind::WebTransport, Admission::Session, &["CONNECT"]),
        WtUpload => ("wtUpload", "/wt/upload", Kind::WebTransport, Admission::Session, &["CONNECT"]),
        WtPing => ("wtPing", "/wt/ping", Kind::WebTransport, Admission::Request, &["CONNECT"]),
        Servers => ("servers", "/servers", Kind::Http, Admission::Unmetered, &["GET"]),
        UploadCheckpoint => ("uploadCheckpoint", "/upload/checkpoint", Kind::Http, Admission::Unmetered, &["POST"]),
    }
}

impl Route {
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
    Route::ALL.into_iter().find(|route| route.path() == path)
}

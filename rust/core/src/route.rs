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
    const fn row(self) -> (&'static str, &'static str, Kind) {
        match self {
            Self::Preflight => ("preflight", "/preflight", Kind::Http),
            Self::Probe => ("probe", "/probe", Kind::Http),
            Self::Download => ("download", "/download", Kind::Http),
            Self::Upload => ("upload", "/upload", Kind::Http),
            Self::UploadSession => ("uploadSession", "/upload/session", Kind::Http),
            Self::UploadProgress => ("uploadProgress", "/upload/progress", Kind::Http),
            Self::WtSession => ("wtSession", "/wt/session", Kind::Http),
            Self::WsSession => ("wsSession", "/ws/session", Kind::Http),
            Self::Ping => ("ping", "/ws/ping", Kind::WebSocket),
            Self::WtDownload => ("wtDownload", "/wt/download", Kind::WebTransport),
            Self::WtUpload => ("wtUpload", "/wt/upload", Kind::WebTransport),
            Self::WtPing => ("wtPing", "/wt/ping", Kind::WebTransport),
            Self::Servers => ("servers", "/servers", Kind::Http),
            Self::UploadCheckpoint => ("uploadCheckpoint", "/upload/checkpoint", Kind::Http),
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
}

/// Matches the entire path, without URL decoding or slash normalization.
pub fn lookup(path: &str) -> Option<Route> {
    ALL.into_iter().find(|route| route.path() == path)
}

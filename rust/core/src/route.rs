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
    pub const fn path(self) -> &'static str {
        match self {
            Self::Preflight => "/preflight",
            Self::Probe => "/probe",
            Self::Download => "/download",
            Self::Upload => "/upload",
            Self::UploadSession => "/upload/session",
            Self::UploadProgress => "/upload/progress",
            Self::WtSession => "/wt/session",
            Self::WsSession => "/ws/session",
            Self::Ping => "/ws/ping",
            Self::WtDownload => "/wt/download",
            Self::WtUpload => "/wt/upload",
            Self::WtPing => "/wt/ping",
            Self::Servers => "/servers",
            Self::UploadCheckpoint => "/upload/checkpoint",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Probe => "probe",
            Self::Download => "download",
            Self::Upload => "upload",
            Self::UploadSession => "uploadSession",
            Self::UploadProgress => "uploadProgress",
            Self::WtSession => "wtSession",
            Self::WsSession => "wsSession",
            Self::Ping => "ping",
            Self::WtDownload => "wtDownload",
            Self::WtUpload => "wtUpload",
            Self::WtPing => "wtPing",
            Self::Servers => "servers",
            Self::UploadCheckpoint => "uploadCheckpoint",
        }
    }

    pub const fn kind(self) -> Kind {
        match self {
            Self::Ping => Kind::WebSocket,
            Self::WtDownload | Self::WtUpload | Self::WtPing => Kind::WebTransport,
            _ => Kind::Http,
        }
    }
}

/// Matches the entire path, without URL decoding or slash normalization.
pub fn lookup(path: &str) -> Option<Route> {
    ALL.into_iter().find(|route| route.path() == path)
}

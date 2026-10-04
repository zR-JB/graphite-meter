//! The fixed routes both binaries share (`api/routes.txt`).

/// The transport that reaches a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Http,
    WebSocket,
    /// An HTTP/3 extended-CONNECT session.
    WebTransport,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::WebSocket => "ws",
            Self::WebTransport => "wt",
        }
    }
}

const GET: &[&str] = &["GET"];
const POST: &[&str] = &["POST"];
const CONNECT: &[&str] = &["CONNECT"];

table! {
    /// A route: its key, the exact path it is mounted at, its transport and the methods it dispatches.
    pub enum Route: (&'static str, &'static str, Kind, &'static [&'static str]) {
        Preflight => ("preflight", "/preflight", Kind::Http, GET),
        Probe => ("probe", "/probe", Kind::Http, GET),
        Download => ("download", "/download", Kind::Http, GET),
        Upload => ("upload", "/upload", Kind::Http, POST),
        UploadSession => ("uploadSession", "/upload/session", Kind::Http, POST),
        UploadProgress => ("uploadProgress", "/upload/progress", Kind::Http, &["GET", "DELETE"]),
        WtSession => ("wtSession", "/wt/session", Kind::Http, POST),
        WsSession => ("wsSession", "/ws/session", Kind::Http, POST),
        Ping => ("ping", "/ws/ping", Kind::WebSocket, GET),
        WtDownload => ("wtDownload", "/wt/download", Kind::WebTransport, CONNECT),
        WtUpload => ("wtUpload", "/wt/upload", Kind::WebTransport, CONNECT),
        WtPing => ("wtPing", "/wt/ping", Kind::WebTransport, CONNECT),
        Servers => ("servers", "/servers", Kind::Http, GET),
        UploadCheckpoint => ("uploadCheckpoint", "/upload/checkpoint", Kind::Http, POST),
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

    /// The methods the route dispatches; the router answers HEAD and OPTIONS itself, never as a CORS grant.
    pub const fn methods(self) -> &'static [&'static str] {
        self.row().3
    }

    /// The route mounted at exactly `path`, without URL decoding or slash normalization.
    pub fn from_path(path: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|route| route.path() == path)
    }
}

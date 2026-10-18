//! The fixed routes the server mounts and the clients request (`api/routes.txt`).

table! {
    pub enum Kind {
        name.0: &'static str,
    } {
        Http => ("http",),
        WebSocket => ("ws",),
        WebTransport => ("wt",),
    }
}

const GET: &[&str] = &["GET"];
const POST: &[&str] = &["POST"];
const CONNECT: &[&str] = &["CONNECT"];

table! {
    /// A route: its key, the exact path it is mounted at, its transport and the methods it dispatches.
    pub enum Route {
        name.0: &'static str,
        path.1: &'static str,
        kind.2: Kind,
        /// The methods the route dispatches; the router answers HEAD and OPTIONS itself, never as a CORS grant.
        methods.3: &'static [&'static str],
    } {
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

table! {
    /// A sign-in route, answered only at the public HTTPS origin: its key, exact path and the method it serves.
    pub enum AuthRoute {
        name.0: &'static str,
        path.1: &'static str,
        method.2: &'static str,
    } {
        Login => ("login", "/login", "GET"),
        Password => ("password", "/auth/password", "POST"),
        OidcStart => ("oidcStart", "/auth/oidc/start", "POST"),
        OidcCallback => ("oidcCallback", "/auth/oidc/callback", "GET"),
        Session => ("session", "/auth/session", "GET"),
        Cli => ("cli", "/auth/cli", "GET"),
        Browser => ("browser", "/auth/browser", "GET"),
        CliApprove => ("cliApprove", "/auth/cli/approve", "POST"),
        BrowserApprove => ("browserApprove", "/auth/browser/approve", "POST"),
        CliToken => ("cliToken", "/auth/cli/token", "POST"),
        BrowserToken => ("browserToken", "/auth/browser/token", "POST"),
        Logout => ("logout", "/auth/logout", "POST"),
    }
}

//! Server budget, method and CORS metadata, as Go's route catalogue and mux define them.

use crate::admission::Class;
use graphite_meter_core::route::Route;
use http::Method;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    pub admission: Option<Class>,
    methods: &'static [&'static str],
    /// Go's mux dispatches these: GET also serves HEAD, and plain HTTP routes answer OPTIONS.
    pub allow: &'static str,
}

impl Spec {
    /// Preflight permission; HEAD and OPTIONS remain excluded.
    pub fn allows_cors_method(self, method: &str) -> bool {
        self.methods.contains(&method)
    }

    pub fn serves(self, method: &Method) -> bool {
        self.allow.split(", ").any(|allowed| allowed == method.as_str())
    }
}

pub const fn spec(route: Route) -> Spec {
    let admission = match route {
        Route::Download | Route::Upload | Route::UploadProgress | Route::Ping | Route::WtPing => Some(Class::Request),
        Route::WtDownload | Route::WtUpload => Some(Class::Session),
        _ => None,
    };
    let (methods, allow): (&'static [&'static str], _) = match route {
        Route::Upload | Route::UploadCheckpoint | Route::UploadSession | Route::WtSession | Route::WsSession => {
            (&["POST"], "OPTIONS, POST")
        }
        Route::UploadProgress => (&["GET", "DELETE"], "DELETE, GET, HEAD, OPTIONS"),
        Route::WtDownload | Route::WtUpload | Route::WtPing => (&["CONNECT"], "CONNECT"),
        Route::Ping => (&["GET"], "GET, HEAD"),
        _ => (&["GET"], "GET, HEAD, OPTIONS"),
    };
    Spec {
        admission,
        methods,
        allow,
    }
}

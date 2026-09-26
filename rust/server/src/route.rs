//! Server budget and CORS metadata; handlers retain their own method dispatch.

use crate::admission::Class;
use graphite_meter_core::route::Route;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    pub admission: Option<Class>,
    cors_methods: &'static [&'static str],
}

impl Spec {
    /// Preflight permission only. This must not restrict handler dispatch.
    pub fn allows_cors_method(self, method: &str) -> bool {
        self.cors_methods.contains(&method)
    }
}

pub const fn spec(route: Route) -> Spec {
    let admission = match route {
        Route::Download | Route::Upload | Route::UploadProgress | Route::Ping | Route::WtPing => {
            Some(Class::Request)
        }
        Route::WtDownload | Route::WtUpload => Some(Class::Session),
        _ => None,
    };
    let cors_methods: &'static [&'static str] = match route {
        Route::Upload
        | Route::UploadCheckpoint
        | Route::UploadSession
        | Route::WtSession
        | Route::WsSession => &["POST"],
        Route::UploadProgress => &["GET", "DELETE"],
        Route::WtDownload | Route::WtUpload | Route::WtPing => &["CONNECT"],
        _ => &["GET"],
    };
    Spec {
        admission,
        cors_methods,
    }
}

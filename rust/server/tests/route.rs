use graphite_meter_core::route::{ALL, Route};
use graphite_meter_server::admission::Class;
use graphite_meter_server::route::spec;

#[test]
fn budgets_preserve_ping_request_class() {
    assert_eq!(spec(Route::WtPing).admission, Some(Class::Request));
    for route in [
        Route::Download,
        Route::Upload,
        Route::UploadProgress,
        Route::Ping,
    ] {
        assert_eq!(spec(route).admission, Some(Class::Request));
    }
    for route in [Route::WtDownload, Route::WtUpload] {
        assert_eq!(spec(route).admission, Some(Class::Session));
    }
    for route in [
        Route::Servers,
        Route::UploadCheckpoint,
        Route::Preflight,
        Route::Probe,
        Route::UploadSession,
        Route::WtSession,
        Route::WsSession,
    ] {
        assert_eq!(spec(route).admission, None);
    }
}

#[test]
fn cors_permissions_are_exact_and_exclude_head_options_empty() {
    for route in ALL {
        for method in ["", "HEAD", "OPTIONS", "get", "PATCH"] {
            assert!(!spec(route).allows_cors_method(method));
        }
    }
    let progress = spec(Route::UploadProgress);
    assert!(progress.allows_cors_method("GET"));
    assert!(progress.allows_cors_method("DELETE"));
    assert!(!progress.allows_cors_method("POST"));
    for route in [Route::WtDownload, Route::WtUpload, Route::WtPing] {
        assert!(spec(route).allows_cors_method("CONNECT"));
        assert!(!spec(route).allows_cors_method("GET"));
    }
    for route in [
        Route::Upload,
        Route::UploadCheckpoint,
        Route::UploadSession,
        Route::WtSession,
        Route::WsSession,
    ] {
        assert!(spec(route).allows_cors_method("POST"));
        assert!(!spec(route).allows_cors_method("GET"));
    }
    for route in [
        Route::Servers,
        Route::Preflight,
        Route::Probe,
        Route::Download,
        Route::Ping,
    ] {
        assert!(spec(route).allows_cors_method("GET"));
        assert!(!spec(route).allows_cors_method("POST"));
    }
}

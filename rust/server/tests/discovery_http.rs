use graphite_meter_server::{
    admission::{Admission, Class, Limits},
    config::Config,
    discovery::Discovery,
};
use http::{Request, StatusCode, Version};
use serde_json::Value;
use std::sync::Arc;

fn request(path: &str, method: &str) -> Request<()> {
    Request::builder()
        .uri(path)
        .method(method)
        .header("host", "meter.example:80")
        .body(())
        .unwrap()
}

fn respond(discovery: &Discovery, request: Request<()>) -> http::Response<bytes::Bytes> {
    discovery
        .respond(&request, "192.0.2.8:54321".parse().unwrap())
        .unwrap()
        .unwrap()
}

#[test]
fn methods_and_headers_match_concrete_go_handlers() {
    let discovery = Discovery::new(Arc::new(Config::default()), None, None).unwrap();
    for path in ["/probe", "/preflight"] {
        for method in ["GET", "POST", "HEAD"] {
            let response = respond(&discovery, request(path, method));
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["content-type"], "application/json");
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
    }
    for method in ["POST", "HEAD", "OPTIONS"] {
        let response = respond(&discovery, request("/servers", method));
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(response.body().is_empty());
        assert!(!response.headers().contains_key("content-type"));
    }
    assert!(
        discovery
            .respond(&request("/unknown", "GET"), "127.0.0.1:80".parse().unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn catalogue_uses_identity_dynamic_authority_and_singleton_fallback() {
    let mut config = Config {
        server_name: "Local meter".into(),
        server_location: "Berlin".into(),
        ..Config::default()
    };
    config.server_catalog.servers.clear();
    let discovery = Discovery::new(Arc::new(config), None, None).unwrap();
    for (uri, expected) in [
        ("/servers", "http://meter.example:7246"),
        ("http://[2001:db8::1]/servers", "http://[2001:db8::1]:7246"),
    ] {
        let response = respond(&discovery, request(uri, "GET"));
        let value: Value = serde_json::from_slice(response.body()).unwrap();
        assert_eq!(value["defaultSelection"], serde_json::json!(["self"]));
        assert_eq!(value["servers"][0]["name"], "Local meter");
        assert_eq!(value["servers"][0]["location"], "Berlin");
        assert_eq!(
            value["servers"][0]["additionalOrigins"],
            serde_json::json!([expected])
        );
    }
}

#[test]
fn probe_reports_actual_protocol_load_and_bootstrap_headers() {
    let admission = Admission::new(Limits::default());
    let _permit = admission.acquire(Class::Request, "client").unwrap();
    let discovery =
        Discovery::new(Arc::new(Config::default()), Some(admission), Some(7443)).unwrap();
    let response = respond(&discovery, request("/probe", "GET"));
    assert_eq!(response.headers()["alt-svc"], "h3=\":7443\"");
    assert_eq!(response.headers()["connection"], "close");
    let value: Value = serde_json::from_slice(response.body()).unwrap();
    assert_eq!(value["clientIp"], "192.0.2.8");
    assert_eq!(value["load"]["active"], 1);
    let mut req = request("/probe", "GET");
    *req.version_mut() = Version::HTTP_2;
    let response = respond(&discovery, req);
    assert!(!response.headers().contains_key("alt-svc"));
    let value: Value = serde_json::from_slice(response.body()).unwrap();
    assert_eq!(value["protocolNegotiated"], "h2");
}

#[test]
fn preflight_generation_is_stable_and_authority_is_not_optional() {
    let discovery = Discovery::new(Arc::new(Config::default()), None, None).unwrap();
    let first: Value =
        serde_json::from_slice(respond(&discovery, request("/preflight", "GET")).body()).unwrap();
    let second: Value = serde_json::from_slice(
        respond(&discovery, request("http://other.example/preflight", "GET")).body(),
    )
    .unwrap();
    assert_eq!(first["generation"], second["generation"]);
    assert_eq!(
        second["capabilities"]["throughput"][0]["baseUrl"],
        "http://other.example:7246"
    );
    let req = Request::builder().uri("/preflight").body(()).unwrap();
    assert!(
        discovery
            .respond(&req, "127.0.0.1:80".parse().unwrap())
            .is_err()
    );
}

#[test]
fn oversized_published_catalogue_is_rejected_before_response() {
    use graphite_meter_core::catalog::ServerEntry;
    let mut config = Config::default();
    let label = "a".repeat(50);
    for index in 0..31 {
        config.server_catalog.servers.push(ServerEntry {
            id: format!("peer-{index}"),
            url: format!("https://peer-{index}.example"),
            name: format!("Peer {index}"),
            additional_origins: (0..32)
                .map(|port| format!("https://{label}.{label}.example:{}", 8000 + port))
                .collect(),
            ..ServerEntry::default()
        });
    }
    let discovery = Discovery::new(Arc::new(config), None, None).unwrap();
    let error = discovery
        .respond(&request("/servers", "GET"), "127.0.0.1:80".parse().unwrap())
        .unwrap_err();
    assert_eq!(error.to_string(), "published catalogue exceeds 64 KiB");
}

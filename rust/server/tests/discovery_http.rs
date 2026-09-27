use graphite_meter_server::{config::Config, discovery::Discovery};
use http::{Request, StatusCode};
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
fn preflight_requires_request_authority() {
    let discovery = Discovery::new(Arc::new(Config::default()), None, None).unwrap();
    let req = Request::builder().uri("/preflight").body(()).unwrap();
    assert!(discovery.respond(&req, "127.0.0.1:80".parse().unwrap()).is_err());
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

#[test]
fn invalid_request_hosts_fall_back_to_localhost() {
    let discovery = Discovery::new(Arc::new(Config::default()), None, None).unwrap();
    for host in [
        "bad_name",
        "-bad.example",
        "bad-.example",
        "[fe80::1%eth0]",
        "user@meter.example",
    ] {
        for path in ["/servers", "/preflight"] {
            let request = Request::builder().uri(path).header("host", host).body(()).unwrap();
            let response = respond(&discovery, request);
            assert_eq!(response.status(), StatusCode::OK);
            let text = std::str::from_utf8(response.body()).unwrap();
            assert!(text.contains("http://localhost:7246"), "{host}: {text}");
        }
    }
}

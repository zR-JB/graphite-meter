use graphite_meter_server::{admission::Admission, config::Config, discovery::Discovery};
use http::{Request, StatusCode};
use std::sync::Arc;

fn respond(discovery: &Discovery, path: &str) -> http::Response<bytes::Bytes> {
    let request = Request::get(path).header("host", "meter.example:80").body(()).unwrap();
    let route = graphite_meter_core::route::lookup(request.uri().path()).expect("a discovery route");
    let peer = "192.0.2.8:54321".parse().unwrap();
    discovery.respond(route, &request, peer).unwrap().unwrap()
}

#[test]
fn oversized_published_catalogue_is_withheld_while_preflight_answers() {
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
    let config = Arc::new(config.validated().unwrap());
    let discovery = Discovery::new(config, Admission::new(Default::default())).unwrap();
    let servers = respond(&discovery, "/servers");
    assert_eq!(servers.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(servers.body(), "server catalogue unavailable\n");
    assert_eq!(respond(&discovery, "/preflight").status(), StatusCode::OK);
}

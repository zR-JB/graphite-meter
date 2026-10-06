//! Discovery, the probe, downloads, handler limits and socket tickets.

use super::*;
use graphite_meter_proto::{
    catalog::{ServerCatalog, ServerId},
    discovery::{Preflight, Probe},
    origin::{BaseUrl, Origin},
};
use graphite_meter_server::config::ENGINE_VERSION;
use http::StatusCode;
use serde_json::json;

fn golden(name: &str) -> Value {
    let path = format!("{}/../../api/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

const GOLDEN: [(&str, &str); 7] = [
    ("GM_TLS_CERT", "/cert.pem"),
    ("GM_TLS_KEY", "/key.pem"),
    ("GM_H1_PUBLIC_ORIGIN", "http://speed.example:7246"),
    ("GM_H3_ADDR", ":7249"),
    ("GM_H3_PUBLIC_ORIGIN", "https://speed.example:7249"),
    ("GM_PUBLIC_ORIGINS", "self"),
    ("GM_SERVER_LOCATION", "fra"),
];

#[tokio::test]
async fn the_preflight_matches_the_golden_document_with_a_fixed_generation() {
    let app = app(&GOLDEN);
    let preflight = || send(&app, Endpoint::H1, empty(request("GET", "/preflight")));
    let response = preflight().await;
    assert_eq!(header(&response, "content-type"), Some("application/json"));
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    let document = json(response).await;
    let generation = document["generation"].as_str().unwrap().to_owned();
    assert!(generation.len() == 32 && generation.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
    let mut expected = golden("preflight.golden.json");
    expected["engineVersion"] = ENGINE_VERSION.into();
    expected["generation"] = generation.clone().into();
    assert_eq!(document, expected);
    assert!(Preflight::decode(document.to_string().as_bytes()).is_ok());
    assert_eq!(json(preflight().await).await["generation"], generation.as_str());
    let restarted = super::app(&GOLDEN);
    let other = json(send(&restarted, Endpoint::H1, empty(request("GET", "/preflight"))).await).await;
    assert_ne!(other["generation"], generation.as_str());
}

#[tokio::test]
async fn servers_publishes_the_catalogue_with_self_approving_its_own_targets() {
    let app = app(&[("GM_SERVER_CATALOG", r#"["https://other.example"]"#)]);
    let response = send(&app, Endpoint::H1, empty(request("GET", "/servers"))).await;
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    let catalog = ServerCatalog::decode(text(response).await.as_bytes()).unwrap().catalog;
    assert_eq!(catalog.default_selection, [ServerId::own()]);
    assert_eq!(catalog.servers.len(), 2);
    let own = &catalog.servers[0];
    assert_eq!(own.url, BaseUrl::Served);
    assert_eq!(own.additional_origins, [Origin::parse("http://speed.example:7246").unwrap()]);
}

#[tokio::test]
async fn the_probe_reports_the_client_the_transport_and_the_handler_load() {
    let env = [&ALL_LISTENERS[..], &[("GM_TRUSTED_PROXIES", "10.0.0.0/8")]].concat();
    let app = &app(&env);
    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(send_from(app, Endpoint::H2, "192.0.2.7", empty(request("GET", "/download?bytes=1000"))).await);
    }
    let probe = |endpoint, peer| async move {
        let response = send_from(app, endpoint, peer, empty(request("GET", "/probe"))).await;
        json(response).await
    };
    let document = probe(Endpoint::Quic, "::ffff:198.51.100.4").await;
    assert_eq!(document, golden("probe.golden.json"));
    assert!(Probe::decode(document.to_string().as_bytes()).is_ok());
    drop(held);
    for (endpoint, protocol) in [(Endpoint::H1, "http/1.1"), (Endpoint::H2, "h2"), (Endpoint::H3Companion, "http/1.1")]
    {
        let document = probe(endpoint, "2001:db8::9").await;
        let expected = json!({"clientIp": "2001:db8::9", "clientIpVersion": 6, "clientIpSource": "socket",
            "protocolNegotiated": protocol, "load": {"active": 0, "max": 256}});
        assert_eq!(document, expected);
    }
    let forwarded = request("GET", "/probe").header("x-real-ip", "198.51.100.8");
    let document = json(send_from(app, Endpoint::H1, "10.1.2.3", empty(forwarded)).await).await;
    assert_eq!(
        (&document["clientIp"], &document["clientIpSource"]),
        (&json!("198.51.100.8"), &json!("forwarded"))
    );
}

#[tokio::test]
async fn full_handler_pools_and_client_shares_ask_for_a_retry() {
    let limits = [
        ("GM_MAX_ACTIVE_MEASUREMENTS", "2"),
        ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "1"),
        ("GM_MAX_ACTIVE_SESSIONS", "1"),
        ("GM_MAX_SESSIONS_PER_CLIENT", "1"),
        ("GM_TRUSTED_PROXIES", "10.0.0.0/8"),
    ];
    let app = app(&limits);
    let download = |peer| send_from(&app, Endpoint::H1, peer, empty(request("GET", "/download?bytes=1")));
    let held = download("192.0.2.1").await;
    let response = download("192.0.2.1").await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        (header(&response, "retry-after"), header(&response, "access-control-allow-origin")),
        (Some("1"), Some("*"))
    );
    let other = download("192.0.2.2").await;
    assert_eq!(other.status(), StatusCode::OK);
    let response = download("192.0.2.3").await;
    assert_eq!(
        (response.status(), header(&response, "retry-after")),
        (StatusCode::SERVICE_UNAVAILABLE, Some("1"))
    );
    let response = download("10.0.0.1").await;
    assert_eq!(
        (response.status(), text(response).await.as_str()),
        (StatusCode::BAD_REQUEST, "ambiguous client address\n")
    );
    drop((held, other));
    assert_eq!(download("192.0.2.3").await.status(), StatusCode::OK);
}

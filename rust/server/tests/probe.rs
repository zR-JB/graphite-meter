use graphite_meter_core::discovery::{ClientIpSource, Probe as Document};
use graphite_meter_server::{config::Config, probe::Probe};
use http::{HeaderMap, Version};
use std::sync::Arc;

#[test]
fn bootstrap_headers_apply_only_to_http1_and_forwarding_requires_trust() {
    let config = Config {
        trusted_proxies: vec!["10.0.0.0/8".parse().unwrap()],
        ..Config::default()
    };
    let probe = Probe::new(Arc::new(config), Some(7249), None);
    let mut headers = HeaderMap::new();
    headers.insert(
        "forwarded",
        "for=\"[2001:db8::4]:4567\";proto=https".parse().unwrap(),
    );
    let peer = "10.0.0.2:1234".parse().unwrap();
    assert_eq!(
        probe
            .respond(peer, Version::HTTP_11, &headers)
            .unwrap()
            .status(),
        400
    );
    headers.remove("forwarded");
    headers.insert("x-real-ip", "2001:db8::4".parse().unwrap());
    let response = probe.respond(peer, Version::HTTP_11, &headers).unwrap();
    assert_eq!(response.headers()["alt-svc"], "h3=\":7249\"");
    assert_eq!(response.headers()["connection"], "close");
    let document = Document::decode(response.body()).unwrap();
    assert_eq!(document.client_ip, "2001:db8::4");
    assert_eq!(document.client_ip_version, 6);
    assert_eq!(document.client_ip_source, ClientIpSource::Forwarded);
    assert!(document.load.is_none());
    for version in [Version::HTTP_2, Version::HTTP_3] {
        let response = probe.respond(peer, version, &headers).unwrap();
        assert!(!response.headers().contains_key("alt-svc"));
        assert!(!response.headers().contains_key("connection"));
    }
}

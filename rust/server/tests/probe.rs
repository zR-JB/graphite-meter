use graphite_meter_core::discovery::{ClientIpSource, Probe as Document, ProtocolNegotiated};
use graphite_meter_server::{
    admission::{Admission, Class, Limits},
    config::Config,
    probe::Probe,
};
use http::{HeaderMap, Version};
use std::sync::Arc;

#[test]
fn probe_reports_actual_protocol_and_shared_admission_occupancy() {
    let admission = Admission::new(Limits::default());
    let permit = admission.acquire(Class::Session, "test-client").unwrap();
    let probe = Probe::new(Arc::new(Config::default()), None, Some(admission));
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-proto", "h3".parse().unwrap());
    headers.insert("x-real-ip", "203.0.113.4".parse().unwrap());
    for (version, expected) in [
        (Version::HTTP_11, ProtocolNegotiated::Http1),
        (Version::HTTP_2, ProtocolNegotiated::Http2),
        (Version::HTTP_3, ProtocolNegotiated::Http3),
    ] {
        let response = probe
            .respond("198.51.100.4:1234".parse().unwrap(), version, &headers)
            .unwrap();
        let document = Document::decode(response.body()).unwrap();
        assert_eq!(document.protocol_negotiated, expected);
        assert_eq!(document.client_ip, "198.51.100.4");
        assert_eq!(document.client_ip_source, ClientIpSource::Socket);
        assert_eq!(document.load.unwrap().active, 1);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    drop(permit);
    let response = probe
        .respond(
            "198.51.100.4:1234".parse().unwrap(),
            Version::HTTP_3,
            &headers,
        )
        .unwrap();
    assert_eq!(
        Document::decode(response.body())
            .unwrap()
            .load
            .unwrap()
            .active,
        0
    );
}

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

use graphite_meter_core::discovery::ClientIpSource;
use graphite_meter_server::client_address::{client_keys, resolve};
use http::{HeaderMap, HeaderValue};
use ipnet::IpNet;

#[test]
fn anonymous_ipv6_addresses_share_the_subnet_budget() {
    let key = |peer: &str| client_keys(resolve(peer.parse().unwrap(), &HeaderMap::new(), &[]).addr).remove(0);
    assert_eq!(key("[2001:db8:1:2::1]:9"), "2001:db8:1:2::/64");
    assert_eq!(key("[2001:db8:1:2::1]:9"), key("[2001:db8:1:2::ffff]:10"));
    assert_ne!(key("[2001:db8:1:2::1]:9"), key("[2001:db8:1:3::1]:9"));
    assert_eq!(key("[::ffff:192.0.2.1]:9"), key("192.0.2.1:10"));
}

fn trusted() -> Vec<IpNet> {
    ["10.0.0.0/8", "192.0.2.0/24", "::1/128"]
        .into_iter()
        .map(|s| s.parse().unwrap())
        .collect()
}

#[test]
fn trusted_proxy_evidence_is_single_and_exact() {
    let peer = "10.0.0.2:1234".parse().unwrap();
    let mut headers = HeaderMap::new();
    assert!(!resolve(peer, &headers, &trusted()).usable);
    headers.insert("x-real-ip", HeaderValue::from_static(" 203.0.113.4 "));
    let client = resolve(peer, &headers, &trusted());
    assert!(client.usable);
    assert_eq!(client.addr.to_string(), "203.0.113.4");
    assert_eq!(client.source, ClientIpSource::Forwarded);
    for name in ["forwarded", "x-forwarded-for"] {
        headers.insert(name, HeaderValue::from_static("203.0.113.5"));
        assert!(!resolve(peer, &headers, &trusted()).usable);
        headers.remove(name);
    }
    headers.append("x-real-ip", HeaderValue::from_static("203.0.113.4"));
    assert!(!resolve(peer, &headers, &trusted()).usable);
    let client = resolve("198.51.100.9:9".parse().unwrap(), &headers, &trusted());
    assert!(client.usable);
    assert_eq!(client.source, ClientIpSource::Socket);
}

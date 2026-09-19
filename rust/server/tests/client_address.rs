use graphite_meter_core::discovery::ClientIpSource;
use graphite_meter_server::client_address::resolve;
use http::{HeaderMap, HeaderValue};
use ipnet::IpNet;

#[test]
fn anonymous_ipv6_addresses_share_the_subnet_budget() {
    let key = |peer: &str| resolve(peer.parse().unwrap(), &HeaderMap::new(), &[]).anonymous_key();
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
fn go_address_and_chain_vectors() {
    for (peer, name, value, expected, forwarded) in [
        (
            "198.51.100.9:1234",
            "x-forwarded-for",
            "203.0.113.4",
            "198.51.100.9",
            false,
        ),
        (
            "[2001:db8::9]:1234",
            "x-forwarded-for",
            "",
            "2001:db8::9",
            false,
        ),
        (
            "10.0.0.2:1234",
            "forwarded",
            "for=\"203.0.113.4:4567\";proto=https",
            "203.0.113.4",
            true,
        ),
        (
            "10.0.0.2:1234",
            "forwarded",
            "for=\"[2001:db8::4]:4567\"",
            "2001:db8::4",
            true,
        ),
        (
            "10.0.0.2:1234",
            "x-forwarded-for",
            "[2001:db8::4]",
            "2001:db8::4",
            true,
        ),
        (
            "10.0.0.2:1234",
            "x-real-ip",
            "203.0.113.4",
            "203.0.113.4",
            true,
        ),
        (
            "10.0.0.2:1234",
            "forwarded",
            "for=203.0.113.4;proto=https, for=192.0.2.7",
            "203.0.113.4",
            true,
        ),
        (
            "10.0.0.2:1234",
            "x-forwarded-for",
            "203.0.113.4, 198.51.100.8, 192.0.2.7",
            "198.51.100.8",
            true,
        ),
        (
            "10.0.0.2:1234",
            "x-forwarded-for",
            "192.0.2.3, 192.0.2.7",
            "192.0.2.3",
            true,
        ),
        (
            "[::ffff:10.0.0.2]:1234",
            "x-forwarded-for",
            "::ffff:203.0.113.4",
            "203.0.113.4",
            true,
        ),
        (
            "10.0.0.2:1234",
            "x-forwarded-for",
            "unknown",
            "10.0.0.2",
            false,
        ),
        (
            "10.0.0.2:1234",
            "forwarded",
            "for=_hidden",
            "10.0.0.2",
            false,
        ),
        (
            "10.0.0.2:1234",
            "forwarded",
            "by=\"a,b;c\"; FOR=203.0.113.4",
            "203.0.113.4",
            true,
        ),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_str(value).unwrap());
        let result = resolve(peer.parse().unwrap(), &headers, &trusted());
        assert_eq!(result.addr.to_string(), expected, "{value}");
        assert_eq!(
            result.source,
            if forwarded {
                ClientIpSource::Forwarded
            } else {
                ClientIpSource::Socket
            }
        );
        assert_eq!(result.version(), if expected.contains(':') { 6 } else { 4 });
    }
}

#[test]
fn precedence_uses_first_value_without_fallback_on_malformed() {
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.8"));
    headers.insert("forwarded", HeaderValue::from_static("for=203.0.113.4"));
    headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.9"));
    let peer = "10.0.0.2:1234".parse().unwrap();
    assert_eq!(
        resolve(peer, &headers, &trusted()).addr.to_string(),
        "198.51.100.9"
    );
    headers.insert("x-real-ip", HeaderValue::from_static("bad"));
    headers.append("x-real-ip", HeaderValue::from_static("203.0.113.5"));
    assert_eq!(
        resolve(peer, &headers, &trusted()).source,
        ClientIpSource::Socket
    );
    headers.remove("x-real-ip");
    headers.insert("forwarded", HeaderValue::from_static("for=unknown"));
    assert_eq!(
        resolve(peer, &headers, &trusted()).source,
        ClientIpSource::Socket
    );
    headers.insert("forwarded", HeaderValue::from_static(""));
    assert_eq!(
        resolve(peer, &headers, &trusted()).addr.to_string(),
        "198.51.100.8"
    );
}

#[test]
fn quoted_go_escapes_and_rejected_ambiguous_addresses() {
    for value in [r#""\x32\060\u0033.0.113.4""#, r#""\U0000003203.0.113.4""#] {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_str(value).unwrap());
        assert_eq!(
            resolve("10.0.0.2:1".parse().unwrap(), &headers, &trusted())
                .addr
                .to_string(),
            "203.0.113.4"
        );
    }
    for value in [
        r#""203.0.113.4\q""#,
        r#""\40003.0.113.4""#,
        r#""\uD800""#,
        "[fe80::1%eth0]",
        "[fe80::1%eth0]:80",
        "203.0.113.4:65536",
        "203.0.113.4:-1",
        "203.0.113.4:+80",
        "010.0.0.1",
        "\"203.0.113.4",
        "\"203.0.113.4\"junk",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_str(value).unwrap());
        assert_eq!(
            resolve("10.0.0.2:1".parse().unwrap(), &headers, &trusted()).source,
            ClientIpSource::Socket,
            "{value}"
        );
    }
    for value in [
        "for=203.0.113.4,",
        "for=203.0.113.4;by=\"unterminated",
        "by=203.0.113.4",
        "for=unknown;for=203.0.113.4",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("forwarded", HeaderValue::from_str(value).unwrap());
        assert_eq!(
            resolve("10.0.0.2:1".parse().unwrap(), &headers, &trusted()).source,
            ClientIpSource::Socket,
            "{value}"
        );
    }
}

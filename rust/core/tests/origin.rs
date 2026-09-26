use graphite_meter_core::origin::{canonical_origin, split_url, target_origin};

#[test]
fn identities_keep_go_port_and_ipv6_spelling() {
    for (raw, expected) in [
        ("HTTPS://Meter.Example:443", "https://meter.example"),
        ("http://meter.example:80", "http://meter.example"),
        ("https://[2001:DB8:0:0::1]:443", "https://[2001:db8:0:0::1]"),
        ("https://meter.example:0443", "https://meter.example:0443"),
        ("https://meter.example:", "https://meter.example"),
        ("https://meter.example.", "https://meter.example."),
    ] {
        assert_eq!(canonical_origin(raw).unwrap(), expected);
    }
    assert!(target_origin(".").unwrap().is_none());
    assert!(canonical_origin(".").is_err());
}

#[test]
fn audiences_reject_paths_credentials_and_ambiguous_authorities() {
    for raw in [
        "",
        "https://",
        "https://a/",
        "https://a?",
        "https://a#",
        "https://a/path",
        "https://user@a",
        "https://a\\b",
        "https://a:0",
        "https://a:000",
        "https://a:65536",
        "https://a:*",
        "https://*.example",
        "https://a;example",
        "https://::1",
        "https://[127.0.0.1]",
        "https://[fe80::1%25eth0]",
        " https://a",
        "https://a\n",
    ] {
        assert!(canonical_origin(raw).is_err(), "accepted {raw:?}");
    }
}

#[test]
fn origins_reject_hosts_that_http_url_parsing_reinterprets() {
    for raw in [
        "https://1.2.3",
        "https://0x7f.1",
        "https://0x7f000001",
        "https://127.000.0.1",
        "https://127.0.0.1.",
        "https://BÜCHER.example",
        "https://a..b",
        "https://.a",
        "https://a!b",
    ] {
        assert!(canonical_origin(raw).is_err(), "accepted {raw:?}");
    }
    assert_eq!(
        canonical_origin("https://XN--BCHER-KVA.example").unwrap(),
        "https://xn--bcher-kva.example"
    );
}

#[test]
fn absolute_urls_keep_ascii_paths_and_queries_without_credentials_or_fragments() {
    for (raw, expected_origin, expected_rest) in [
        (
            "HTTPS://Id.Example:8443/realms/x?a=b",
            "https://id.example:8443",
            "/realms/x?a=b",
        ),
        ("https://id.example?x=1", "https://id.example", "?x=1"),
        ("https://id.example", "https://id.example", ""),
        ("https://id.example/%C3%A4", "https://id.example", "/%C3%A4"),
    ] {
        let (origin, rest) = split_url(raw).unwrap();
        assert_eq!(origin.key(), expected_origin);
        assert_eq!(rest, expected_rest);
    }
    for raw in [
        "https://id.example/a#b",
        "https://user@id.example/a",
        "https://id.example/ä",
        "https://id.example/a b",
        "https://id.example\\a",
        "ftp://id.example/a",
    ] {
        assert!(split_url(raw).is_err(), "accepted {raw:?}");
    }
    assert!(split_url(&format!("https://id.example/{}", "a".repeat(2048))).is_err());
}

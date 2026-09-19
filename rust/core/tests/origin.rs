use graphite_meter_core::origin::{canonical_origin, target_origin};

#[test]
fn identities_keep_go_port_and_ipv6_spelling() {
    for (raw, expected) in [
        ("HTTPS://Meter.Example:443", "https://meter.example"),
        ("http://meter.example:80", "http://meter.example"),
        ("https://[2001:DB8:0:0::1]:443", "https://[2001:db8:0:0::1]"),
        ("https://meter.example:0443", "https://meter.example:0443"),
        ("https://meter.example:", "https://meter.example"),
        ("https://BÜCHER.example", "https://bücher.example"),
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

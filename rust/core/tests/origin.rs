use graphite_meter_core::origin::{canonical_origin, catalog_origin, split_url, target_origin};

/// Go's CatalogOrigin: one trailing slash goes, and international hosts become the punycode
/// Go's client dials; configured origins keep refusing both.
#[test]
fn catalogue_origins_take_one_slash_and_international_hosts() {
    for (raw, expected) in [
        ("https://meter.example/", "https://meter.example"),
        ("HTTPS://Meter.Example:8443/", "https://meter.example:8443"),
        ("https://BÜCHER.example", "https://xn--bcher-kva.example"),
        ("https://münchen.example:7248/", "https://xn--mnchen-3ya.example:7248"),
        ("https://straße.example", "https://xn--strae-oqa.example"),
        ("https://例え.テスト", "https://xn--r8jz45g.xn--zckzah"),
        ("https://παράδειγμα.δοκιμή", "https://xn--hxajbheg2az3al.xn--jxalpdlp"),
        ("https://ПРИМЕР.испытание", "https://xn--e1afmkfd.xn--80akhbyknj4f"),
        ("https://실례.테스트", "https://xn--9n2bp8q.xn--9t4b11yi5a"),
        ("https://مثال.إختبار", "https://xn--mgbh0fb.xn--kgbechtv"),
        ("https://עברית.example.", "https://xn--5dbqzzl.example."),
        ("https://ყველა.example", "https://xn--lodhcv6d.example"),
    ] {
        assert_eq!(catalog_origin(raw).as_deref(), Ok(expected), "{raw}");
    }
    for raw in [
        "https://meter.example//",
        "https://meter.example/path",
        // Forms IDNA maps to other letters, and what it refuses.
        "https://ｍｅｔｅｒ.example",
        "https://ℌeter.example",
        "https://ﬁle.example",
        "https://e\u{301}.example",
        "https://İstanbul.example",
        "https://💩.example",
        "https://-bücher.example",
        "https://bü--cher.example",
        "https://a\u{5d0}.example",
        "https://bü cher.example",
        "https://bücher.example:x",
        "https://ü@meter.example",
    ] {
        assert!(catalog_origin(raw).is_err(), "accepted {raw:?}");
    }
    assert!(canonical_origin("https://bücher.example").is_err());
    assert!(canonical_origin("https://meter.example/").is_err());
}

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
        // Hosts that HTTP URL parsing reinterprets.
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
        ("HTTPS://Id.Example:8443/realms/x?a=b", "https://id.example:8443", "/realms/x?a=b"),
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

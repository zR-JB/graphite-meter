use graphite_meter_proto::origin::{BaseUrl, Host, Origin};

fn canonical(text: &str) -> Option<String> {
    Origin::parse(text).ok().map(|origin| origin.to_string())
}

#[test]
fn origins_compare_and_print_in_canonical_form() {
    for (text, expected) in [
        ("HTTPS://Meter.Example:443", "https://meter.example"),
        ("http://meter.example:80", "http://meter.example"),
        ("https://meter.example:08443", "https://meter.example:8443"),
        ("https://meter.example:0443", "https://meter.example"),
        ("http://meter.example:443", "http://meter.example:443"),
        ("https://meter.example.", "https://meter.example."),
        ("https://XN--BCHER-KVA.example", "https://xn--bcher-kva.example"),
        ("https://under_score.example", "https://under_score.example"),
        ("https://[2001:DB8:0:0::1]:443", "https://[2001:db8::1]"),
        ("http://[::1]:7246", "http://[::1]:7246"),
    ] {
        assert_eq!(canonical(text).as_deref(), Some(expected), "{text}");
    }
    let origin = Origin::parse("https://Meter.Example:8443").unwrap();
    assert_eq!(origin, Origin::parse("https://meter.example:08443").unwrap());
}

#[test]
fn origins_refuse_credentials_paths_queries_fragments_and_bad_ports() {
    for text in [
        "",
        ".",
        "https://a/path",
        "https://user@a",
        "https://a\\b",
        "https://a:0",
        "https://a:65536",
        "ftp://a",
        " https://a",
        "https://a\n",
    ] {
        assert_eq!(canonical(text), None, "accepted {text:?}");
    }
    let long = format!("https://{}.example", "a".repeat(2048));
    assert_eq!(canonical(&long), None);
    let longest = format!("https://{}", "a".repeat(2048 - "https://".len()));
    assert!(canonical(&longest).is_some());
}

#[test]
fn hosts_are_ascii_names_or_full_ip_addresses() {
    for text in [
        "https://*.example",
        "https://a..b",
        "https://bücher.example",
        "https://0x7f000001",
        "https://127.000.0.1",
        "https://example.123",
        "https://::1",
        "https://[fe80::1%25eth0]",
    ] {
        assert_eq!(canonical(text), None, "accepted {text:?}");
    }
    assert_eq!(Origin::parse("http://10.0.0.1").unwrap().host, Host::Ip([10, 0, 0, 1].into()));
}

#[test]
fn urls_split_into_their_origin_and_the_rest() {
    for (url, origin, rest) in [
        ("HTTPS://Id.Example:8443/realms/x?a=b", "https://id.example:8443", "/realms/x?a=b"),
        ("https://id.example", "https://id.example", ""),
    ] {
        let (parsed, tail) = Origin::split(url).unwrap();
        assert_eq!((parsed.to_string().as_str(), tail), (origin, rest), "{url}");
    }
    let long = format!("https://id.example/{}", "a".repeat(2048));
    for url in [
        "https://user@id.example/a",
        "https://id.example#top",
        "https://id.example/realms/é",
        "https://id.example/a b",
        "https://id.example/a\\b",
        &long,
    ] {
        assert!(Origin::split(url).is_err(), "accepted {url:?}");
    }
    assert!(Origin::split(&long[..long.len() - 1]).is_ok());
}

#[test]
fn a_dot_base_url_names_the_origin_that_served_the_document() {
    let served = Origin::parse("https://meter.example").unwrap();
    assert_eq!(BaseUrl::parse(".").unwrap().resolve(&served), &served);
    let other = BaseUrl::parse("https://speed.example:7249").unwrap();
    assert_eq!(other.resolve(&served).to_string(), "https://speed.example:7249");
    assert!(BaseUrl::parse("./").is_err());
    assert_eq!(serde_json::to_string(&BaseUrl::Served).unwrap(), "\".\"");
    assert_eq!(serde_json::to_string(&other).unwrap(), "\"https://speed.example:7249\"");
}

#[test]
fn received_international_hosts_become_the_punycode_go_dials() {
    for (text, expected) in [
        ("https://BÜCHER.example", "https://xn--bcher-kva.example"),
        ("https://münchen.example:7248", "https://xn--mnchen-3ya.example:7248"),
        ("https://例え.テスト", "https://xn--r8jz45g.xn--zckzah"),
        ("https://مثال.إختبار", "https://xn--mgbh0fb.xn--kgbechtv"),
        ("https://עברית.example.", "https://xn--5dbqzzl.example."),
        ("https://σς.example", "https://xn--3xab.example"),
        ("https://café", "https://xn--caf-dma"),
        ("https://xn--bcher-kva.ü", "https://xn--bcher-kva.xn--tda"),
        ("https://中国\u{3002}example", "https://xn--fiqs8s.example"),
    ] {
        let received = Origin::parse_received(text).map(|origin| origin.to_string());
        assert_eq!(received.as_deref(), Ok(expected), "{text}");
        assert!(Origin::parse(text).is_err(), "configured origins keep ASCII hosts: {text}");
    }
    assert_eq!(
        Origin::parse_received("https://Meter.example").unwrap().to_string(),
        "https://meter.example"
    );
}

#[test]
fn received_hosts_go_refuses_or_idna_would_map_are_refused() {
    let go_refuses = ["-bücher.example", "ab--ü.example", "aא.example", "١٢٣.example", "ü_x.example"];
    // Go maps, composes or keeps these; only letters IDNA keeps as they are convert.
    let mapped = [
        "ｍｅｔｅｒ.example",
        "ǅ.example",
        "e\u{301}.example",
        "İstanbul.example",
        "💩.example",
        "bü--cher.example",
    ];
    for host in go_refuses.into_iter().chain(mapped) {
        assert!(Origin::parse_received(&format!("https://{host}")).is_err(), "accepted {host}");
    }
    for text in ["https://ü@meter.example", "https://bücher.example/path", "https://bücher.example:x"] {
        assert!(Origin::parse_received(text).is_err(), "accepted {text}");
    }
    let long = format!("https://ü.{}.example", "a".repeat(2040));
    assert!(Origin::parse_received(&long).is_err());
}

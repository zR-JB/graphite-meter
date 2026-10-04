use graphite_meter_proto::origin::{BaseUrl, Host, Origin, Scheme};

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
        ("https://meter.example:", "https://meter.example"),
        ("http://meter.example:443", "http://meter.example:443"),
        ("https://meter.example.", "https://meter.example."),
        ("https://XN--BCHER-KVA.example", "https://xn--bcher-kva.example"),
        ("https://under_score.example", "https://under_score.example"),
        ("https://[2001:DB8:0:0::1]:443", "https://[2001:db8::1]"),
        ("http://[::1]:7246", "http://[::1]:7246"),
        ("http://127.0.0.1:7246", "http://127.0.0.1:7246"),
        ("https://localhost", "https://localhost"),
    ] {
        assert_eq!(canonical(text).as_deref(), Some(expected), "{text}");
    }
    let origin = Origin::parse("https://Meter.Example:8443").unwrap();
    assert_eq!(origin, Origin::parse("https://meter.example:08443").unwrap());
    let expected = Origin {
        scheme: Scheme::Https,
        host: Host::Name("meter.example".into()),
        port: 8443,
    };
    assert_eq!(origin, expected);
}

#[test]
fn origins_refuse_credentials_paths_queries_fragments_and_bad_ports() {
    for text in [
        "",
        ".",
        "https://",
        "https://a/",
        "https://a?",
        "https://a#",
        "https://a/path",
        "https://user@a",
        "https://user:secret@a",
        "https://a\\b",
        "https://a:0",
        "https://a:000",
        "https://a:65536",
        "https://a:+1",
        "https://a:*",
        "https://a:1:2",
        "ftp://a",
        "wss://a",
        "https:/a",
        " https://a",
        "https://a ",
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
        "https://a;example",
        "https://a!b",
        "https://a..b",
        "https://.a",
        "https://a.example..",
        "https://bücher.example",
        "https://1.2.3",
        "https://0x7f.1",
        "https://0x7f000001",
        "https://2130706433",
        "https://127.000.0.1",
        "https://127.0.0.1.",
        "https://example.123",
        "https://::1",
        "https://[127.0.0.1]",
        "https://[fe80::1%25eth0]",
        "https://[::1",
        "https://[::1]x",
    ] {
        assert_eq!(canonical(text), None, "accepted {text:?}");
    }
    assert_eq!(Origin::parse("http://10.0.0.1").unwrap().host, Host::Ip([10, 0, 0, 1].into()));
}

#[test]
fn urls_split_into_their_origin_and_the_rest() {
    for (url, origin, rest) in [
        ("HTTPS://Id.Example:8443/realms/x?a=b", "https://id.example:8443", "/realms/x?a=b"),
        ("https://id.example?x=1", "https://id.example", "?x=1"),
        ("https://id.example#top", "https://id.example", "#top"),
        ("https://id.example", "https://id.example", ""),
    ] {
        let (parsed, tail) = Origin::split(url).unwrap();
        assert_eq!((parsed.to_string().as_str(), tail), (origin, rest), "{url}");
    }
    assert!(Origin::split("https://user@id.example/a").is_err());
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

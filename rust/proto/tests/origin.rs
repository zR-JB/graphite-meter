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

#[test]
fn received_international_hosts_become_the_punycode_go_dials() {
    for (text, expected) in [
        ("https://BÜCHER.example", "https://xn--bcher-kva.example"),
        ("https://münchen.example:7248", "https://xn--mnchen-3ya.example:7248"),
        ("https://straße.example", "https://xn--strae-oqa.example"),
        ("https://例え.テスト", "https://xn--r8jz45g.xn--zckzah"),
        ("https://παράδειγμα.δοκιμή", "https://xn--hxajbheg2az3al.xn--jxalpdlp"),
        ("https://ПРИМЕР.испытание", "https://xn--e1afmkfd.xn--80akhbyknj4f"),
        ("https://실례.테스트", "https://xn--9n2bp8q.xn--9t4b11yi5a"),
        ("https://مثال.إختبار", "https://xn--mgbh0fb.xn--kgbechtv"),
        ("https://עברית.example.", "https://xn--5dbqzzl.example."),
        ("https://ყველა.example", "https://xn--lodhcv6d.example"),
        ("https://ÄÖÜ.example", "https://xn--4ca0bs.example"),
        ("https://σς.example", "https://xn--3xab.example"),
        ("https://ı.example", "https://xn--cfa.example"),
        ("https://ȸ.example", "https://xn--uma.example"),
        ("https://日本語.jp", "https://xn--wgv71a119e.jp"),
        ("https://가.example", "https://xn--o39a.example"),
        ("https://ア.example", "https://xn--cck.example"),
        ("https://café", "https://xn--caf-dma"),
        ("https://Ж1.example", "https://xn--1-ktb.example"),
        ("https://ü1-2.example", "https://xn--1-2-goa.example"),
        ("https://1ü", "https://xn--1-eha"),
        ("https://ab-ü", "https://xn--ab--joa"),
        ("https://xn--bcher-kva.ü", "https://xn--bcher-kva.xn--tda"),
        ("https://中国\u{3002}example", "https://xn--fiqs8s.example"),
        ("https://ü\u{ff0e}example", "https://xn--tda.example"),
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
    let go_refuses = [
        "-bücher.example",
        "ü-.example",
        "ab--ü.example",
        "aא.example",
        "אa.example",
        "١٢٣.example",
        "bü cher.example",
        "ü_x.example",
        "ä.b_c.example",
        "ü..example",
    ];
    // Go maps, composes or keeps these; only letters IDNA keeps as they are convert.
    let mapped = [
        "ｍｅｔｅｒ.example",
        "ℌeter.example",
        "ﬁle.example",
        "ĳssel.example",
        "ǅ.example",
        "ʰ.example",
        "ⅷ.example",
        "ｱ.example",
        "ㄱ.example",
        "e\u{301}.example",
        "İstanbul.example",
        "שׁ.example",
        "ש1.example",
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

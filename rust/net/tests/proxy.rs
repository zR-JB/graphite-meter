//! Which proxy a target goes through, from the environment by Go's rules.
use graphite_meter_net::Proxy;
use graphite_meter_proto::origin::Origin;

fn from(variables: &[(&str, &str)]) -> Proxy {
    let variables: Vec<(String, String)> = variables.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    Proxy::from_lookup(move |name| {
        variables
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    })
}

/// The proxy `target` goes through, `-` for none, or the refusal.
fn via(proxy: &Proxy, target: &str) -> String {
    match proxy.route(&Origin::parse(target).unwrap()) {
        Ok(Some(upstream)) => upstream.to_string(),
        Ok(None) => "-".into(),
        Err(unusable) => unusable.to_string(),
    }
}

#[test]
fn all_proxy_is_never_read() {
    let everything = from(&[("ALL_PROXY", "http://all.example:3128"), ("all_proxy", "http://all.example:3128")]);
    assert_eq!(
        (via(&everything, "http://meter.example"), via(&everything, "https://meter.example")),
        ("-".into(), "-".into())
    );
}

#[test]
fn each_variable_reads_before_its_lowercase_spelling_and_empty_values_are_unset() {
    let proxy = from(&[
        ("HTTP_PROXY", "http://upper.example:3128"),
        ("http_proxy", "http://lower.example:3128"),
        ("HTTPS_PROXY", ""),
        ("https_proxy", "lower.example:3129"),
        ("NO_PROXY", ""),
        ("no_proxy", "meter.example"),
    ]);
    for (target, expected) in [
        ("http://other.example", "http://upper.example:3128"),
        ("https://other.example", "http://lower.example:3129"),
        ("http://meter.example", "-"),
        ("https://meter.example", "-"),
    ] {
        assert_eq!(via(&proxy, target), expected, "{target}");
    }
}

#[test]
fn proxy_urls_default_to_http_and_socks_to_port_1080() {
    for (value, expected) in [
        ("proxy.example:3128", "http://proxy.example:3128"),
        ("socks5://proxy.example", "socks5://proxy.example:1080"),
        ("SOCKS5H://u:p@proxy.example:1081", "socks5://proxy.example:1081"),
        ("http://bücher.example:8080", "http://xn--bcher-kva.example:8080"),
    ] {
        assert_eq!(via(&from(&[("HTTPS_PROXY", value)]), "https://meter.example"), expected, "{value}");
    }
}

#[test]
fn an_unusable_value_fails_each_request_it_would_carry_naming_the_variable() {
    let proxy = from(&[
        ("https_proxy", "ftp://proxy.example"),
        ("HTTP_PROXY", "http://[bad"),
        ("NO_PROXY", "skip.example"),
    ]);
    assert_eq!(
        via(&proxy, "https://meter.example"),
        "https_proxy is not a usable proxy: only HTTP, HTTPS and SOCKS5 proxies are supported"
    );
    assert_eq!(via(&proxy, "http://meter.example"), "HTTP_PROXY is not a usable proxy: invalid proxy URL");
    assert_eq!(via(&proxy, "https://skip.example"), "-", "a bypassed target is not carried");
    assert_eq!(via(&proxy, "http://127.0.0.1"), "-");
}

#[test]
fn under_cgi_http_proxy_fails_every_cleartext_request_and_https_proxy_still_applies() {
    let proxy = from(&[
        ("REQUEST_METHOD", "GET"),
        ("HTTP_PROXY", "http://attacker.example"),
        ("HTTPS_PROXY", "http://proxy.example:3128"),
        ("NO_PROXY", "meter.example"),
    ]);
    let refused = "HTTP_PROXY is not a usable proxy: a CGI request's Proxy header can set it";
    for target in [
        "http://other.example",
        "http://meter.example",
        "http://127.0.0.1:8080",
        "http://localhost",
    ] {
        assert_eq!(via(&proxy, target), refused, "{target}");
    }
    assert_eq!(via(&proxy, "https://other.example"), "http://proxy.example:3128");
    assert_eq!(via(&proxy, "https://meter.example"), "-");
    let no_proxy_set = from(&[("REQUEST_METHOD", "GET"), ("HTTPS_PROXY", "http://proxy.example:3128")]);
    assert_eq!(via(&no_proxy_set, "http://other.example"), "-", "only a set HTTP_PROXY is refused");
}

#[test]
fn no_proxy_and_loopback_bypass_by_go_s_rules() {
    let no_proxy = concat!(
        "corp.example, .sub.example, *.wild.example, pinned.example:8443, 10.0.0.0/8, 011.0.0.0/8, 192.0.2.7, ",
        "[2001:db8::1]:443, bad:port, ::ffff:198.51.100.1, ::ffff:203.0.113.0/120, [2001:db8::2], ",
        "[2001:db8::3]:, *star.example, BÜCHER.example, padded.example:080, 192.0.2.8:080, .",
    );
    let proxy = from(&[
        ("HTTP_PROXY", "http://proxy.example:3128"),
        ("HTTPS_PROXY", "socks5://proxy.example"),
        ("NO_PROXY", no_proxy),
    ]);
    for (target, bypassed) in [
        ("https://corp.example", true),
        ("https://sub.example", false),
        ("https://a.sub.example", true),
        ("https://a.wild.example", true),
        ("https://pinned.example:8443", true),
        ("http://10.1.2.3", true),
        ("http://192.0.2.7:81", true),
        ("https://[2001:db8::1]", true),
        ("https://[2001:db8::1]:8443", false),
        ("https://meter.example", false),
        ("http://[::ffff:127.0.0.1]", true),
        ("https://198.51.100.1", true),
        ("https://[2001:db8::2]", false),
        ("https://[2001:db8::3]:8443", true),
        ("https://star.example", false),
        ("https://xn--bcher-kva.example", true),
        ("http://192.0.2.8:080", false),
        ("https://meter.example.", true),
    ] {
        assert_eq!(via(&proxy, target) == "-", bypassed, "{target}");
    }
    assert_eq!(
        via(
            &from(&[("HTTPS_PROXY", "proxy.example:1"), ("NO_PROXY", "a.example, *")]),
            "https://b.example"
        ),
        "-"
    );
}

use graphite_meter_core::catalog::{CatalogError, ServerCatalog, ServerEntry};

fn entry(id: &str, url: &str) -> ServerEntry {
    ServerEntry {
        id: id.into(),
        url: url.into(),
        name: id.into(),
        ..ServerEntry::default()
    }
}

#[test]
fn discovery_boundary_is_host_exact_but_port_independent() {
    let mut server = entry("remote", "https://meter.example");
    server
        .additional_origins
        .push("https://transfer.example:7248".into());
    for (raw, allowed) in [
        (".", true),
        ("https://meter.example:7249", true),
        ("https://METER.example", true),
        ("http://meter.example", true),
        ("https://transfer.example:7248", true),
        ("https://transfer.example:7247", false),
        ("https://sub.meter.example", false),
        ("https://user@meter.example", false),
        ("https://meter.example/path", false),
    ] {
        assert_eq!(server.allows_origin(raw), allowed, "{raw}");
    }
}

#[test]
fn ipv6_stays_in_discovery_but_not_csp() {
    let mut catalog = ServerCatalog::default();
    let mut ipv6 = entry("ipv6", "https://[2001:db8::1]");
    ipv6.additional_origins
        .push("https://bulk.example:7249".into());
    let mut dns = entry("dns", "https://meter.example");
    dns.additional_origins
        .push("https://[2001:db8::2]:7248".into());
    catalog.servers.extend([ipv6, dns]);
    catalog.validate().unwrap();
    let sources = catalog.connect_sources();
    assert!(!sources.join(" ").contains('['));
    for source in [
        "https://meter.example:*",
        "wss://meter.example:*",
        "https://bulk.example:7249",
        "wss://bulk.example:7249",
    ] {
        assert!(sources.iter().any(|s| s == source));
    }
    assert!(catalog.servers[1].allows_origin("https://[2001:db8::1]:7249"));
    assert!(catalog.servers[2].allows_origin("https://[2001:db8::2]:7248"));
}

#[test]
fn selection_and_resolution() {
    let mut catalog = ServerCatalog::singleton();
    catalog
        .servers
        .push(entry("remote", "https://REMOTE.example:443"));
    catalog.validate().unwrap();
    for ids in [
        vec![],
        vec!["self", "self"],
        vec!["missing"],
        vec!["self", "remote", "x", "y", "z"],
    ] {
        assert_eq!(
            catalog.validate_selection(&ids.into_iter().map(str::to_owned).collect::<Vec<_>>()),
            Err(CatalogError::InvalidSelection)
        );
    }
    catalog
        .validate_selection(&["remote".into(), "self".into()])
        .unwrap();
    let resolved = catalog.resolve("https://local.example:443");
    assert_eq!(resolved.servers[0].url, "https://local.example");
    assert_eq!(resolved.servers[1].url, "https://remote.example");
    assert_eq!(catalog.servers[0].url, ".");
}

#[test]
fn rejects_duplicate_ids_and_equivalent_origins() {
    for remote in [
        entry("self", "https://remote.example"),
        entry("other", "https://REMOTE.example:443"),
    ] {
        let mut catalog = ServerCatalog::default();
        catalog
            .servers
            .push(entry("remote", "https://remote.example"));
        catalog.servers.push(remote);
        assert_eq!(catalog.validate(), Err(CatalogError::DuplicateServer));
    }
}

#[test]
fn enforces_identity_origin_and_size_limits() {
    let mut catalog = ServerCatalog::default();
    for id in ["".into(), "x".repeat(65), "bad id".into(), "é".into()] {
        let mut invalid = entry(&id, "https://remote.example");
        invalid.name = "valid".into();
        catalog.servers.truncate(1);
        catalog.servers.push(invalid);
        assert_eq!(catalog.validate(), Err(CatalogError::InvalidIdentity));
    }
    for name in [
        "x".repeat(257),
        "é".repeat(129),
        "line\n".into(),
        "del\u{7f}".into(),
    ] {
        let mut invalid = ServerCatalog::default();
        invalid.servers[0].name = name.clone();
        assert_eq!(invalid.validate(), Err(CatalogError::InvalidIdentity));
        invalid.servers[0].name.clear();
        invalid.servers[0].location = name;
        assert_eq!(invalid.validate(), Err(CatalogError::InvalidIdentity));
    }
    for url in [
        ".",
        "https://example/path",
        "https://example:0",
        "https://*.example",
        "https://example;host",
    ] {
        let mut invalid = ServerCatalog::default();
        invalid.servers[0].url = "https://self.example".into();
        invalid.servers.push(entry("remote", url));
        assert_eq!(
            invalid.validate(),
            Err(CatalogError::InvalidOrigin),
            "{url}"
        );
    }
    let mut full = ServerCatalog::default();
    for i in 1..32 {
        full.servers
            .push(entry(&format!("s{i}"), &format!("https://s{i}.example")));
    }
    full.validate().unwrap();
    full.servers
        .push(entry("overflow", "https://overflow.example"));
    assert_eq!(full.validate(), Err(CatalogError::InvalidServers));
    let mut additional = ServerCatalog::default();
    additional.servers[0].additional_origins = vec!["https://extra.example".into(); 32];
    additional.validate().unwrap(); // Additional entries may intentionally repeat; Go allows this.
    additional.servers[0]
        .additional_origins
        .push("https://extra.example".into());
    assert_eq!(
        additional.validate(),
        Err(CatalogError::TooManyAdditionalOrigins)
    );
    additional.servers[0].additional_origins = vec![".".into()];
    assert_eq!(additional.validate(), Err(CatalogError::InvalidOrigin));
    let mut missing = ServerCatalog::default();
    missing.servers.clear();
    assert_eq!(missing.validate(), Err(CatalogError::InvalidServers));
}

#[test]
fn json_shape_and_default_are_public_contract() {
    let value = serde_json::to_value(ServerCatalog::default()).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "defaultSelection": ["self"],
            "servers": [{
                "id": "self",
                "url": ".",
                "name": "graphite-meter"
            }]
        })
    );
    let catalog: ServerCatalog = serde_json::from_value(value).unwrap();
    catalog.validate().unwrap();
    let empty: ServerCatalog = serde_json::from_str("{}").unwrap();
    assert_eq!(empty.validate(), Err(CatalogError::InvalidServers));
}

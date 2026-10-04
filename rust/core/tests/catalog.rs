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
    server.additional_origins.push("https://transfer.example:7248".into());
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
    ipv6.additional_origins.push("https://bulk.example:7249".into());
    let mut dns = entry("dns", "https://meter.example");
    dns.additional_origins.push("https://[2001:db8::2]:7248".into());
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
fn rejects_duplicate_ids_and_equivalent_origins() {
    for remote in [entry("self", "https://remote.example"), entry("other", "https://REMOTE.example:443")] {
        let mut catalog = ServerCatalog::default();
        catalog.servers.push(entry("remote", "https://remote.example"));
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
    for name in ["x".repeat(257), "é".repeat(129), "line\n".into(), "del\u{7f}".into()] {
        let mut invalid = ServerCatalog::default();
        invalid.servers[0].name = name.clone();
        assert_eq!(invalid.validate(), Err(CatalogError::InvalidIdentity));
        invalid.servers[0].name.clear();
        invalid.servers[0].location = name;
        assert_eq!(invalid.validate(), Err(CatalogError::InvalidIdentity));
    }
    for url in [".", "https://example/path"] {
        let mut invalid = ServerCatalog::default();
        invalid.servers[0].url = "https://self.example".into();
        invalid.servers.push(entry("remote", url));
        assert_eq!(invalid.validate(), Err(CatalogError::InvalidOrigin), "{url}");
    }
    let mut full = ServerCatalog::default();
    full.servers
        .extend((1..32).map(|i| entry(&format!("s{i}"), &format!("https://s{i}.example"))));
    full.validate().unwrap();
    full.servers.push(entry("overflow", "https://overflow.example"));
    assert_eq!(full.validate(), Err(CatalogError::InvalidServers));
    let mut additional = ServerCatalog::default();
    additional.servers[0].additional_origins = vec!["https://extra.example".into(); 32];
    additional.validate().unwrap(); // Additional entries may intentionally repeat; Go allows this.
    additional.servers[0].additional_origins = vec!["https://extra.example".into(); 33];
    assert_eq!(additional.validate(), Err(CatalogError::TooManyAdditionalOrigins));
    additional.servers[0].additional_origins = vec![".".into()];
    assert_eq!(additional.validate(), Err(CatalogError::InvalidOrigin));
    let mut missing = ServerCatalog::default();
    missing.servers.clear();
    assert_eq!(missing.validate(), Err(CatalogError::InvalidServers));
    let empty: ServerCatalog = serde_json::from_str("{}").unwrap();
    assert_eq!(empty.validate(), Err(CatalogError::InvalidServers));
}

/// An unusable default selection falls back to self; an invalid self refuses the catalogue.
#[test]
fn a_received_catalogue_requires_a_valid_self_and_selection() {
    // Nothing left to select falls back to self; an invalid self refuses the catalogue.
    let mut only_broken = ServerCatalog::default();
    only_broken.servers.push(entry("broken", "https://two words.example"));
    only_broken.default_selection = vec!["broken".into()];
    assert_eq!(only_broken.received().unwrap().0.default_selection, ["self"]);
    let mut broken_self = ServerCatalog::default();
    broken_self.servers[0].url = "https://two words.example".into();
    assert_eq!(broken_self.received().err(), Some(CatalogError::InvalidOrigin));
}

/// Go reads a JSON null as the zero value, in a field or a list, so a received catalogue loses only
/// the entries its nulls leave invalid.
#[test]
fn a_received_catalogue_reads_null_as_empty() {
    let json = br#"{"defaultSelection": null, "servers": [
        {"id": "self", "url": ".", "name": null, "location": null, "additionalOrigins": null},
        {"id": "listed", "url": "https://listed.example", "name": "listed", "additionalOrigins": [null]},
        null,
        {"id": "remote", "url": "https://remote.example", "name": "remote", "location": null}
    ]}"#;
    let catalog: ServerCatalog = graphite_meter_core::wire::decode_json(json).unwrap();
    assert_eq!(catalog.servers[1].additional_origins, [""]);
    let (received, rejected) = catalog.received().unwrap();
    let ids: Vec<_> = received.servers.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!((ids, received.default_selection), (vec!["self", "remote"], vec!["self".into()]));
    let rejected: Vec<_> = rejected.iter().map(|left| (left.id.as_str(), left.error)).collect();
    assert_eq!(rejected, [("listed", CatalogError::InvalidOrigin), ("", CatalogError::InvalidIdentity)]);
    let nothing: ServerCatalog = graphite_meter_core::wire::decode_json(br#"{"servers": null}"#).unwrap();
    assert_eq!(nothing.received().err(), Some(CatalogError::InvalidServers));
    let array = br#"[["self"],[["self",".","graphite-meter"]]]"#;
    assert!(graphite_meter_core::wire::decode_json::<ServerCatalog>(array).is_err());
}

use graphite_meter_proto::{
    catalog::{CatalogError, Rejected, ServerCatalog, ServerEntry, ServerId},
    discovery::Preflight,
    origin::{BaseUrl, Origin},
};
use serde_json::{Value, json};

fn id(text: &str) -> ServerId {
    ServerId::parse(text).unwrap()
}

fn entry(name: &str, url: &str) -> ServerEntry {
    let url = BaseUrl::parse(url).unwrap();
    ServerEntry {
        id: id(name),
        url,
        name: name.into(),
        location: String::new(),
        additional_origins: Vec::new(),
    }
}

/// `self` and Frankfurt, which may also measure on a transfer host.
fn catalog() -> ServerCatalog {
    let mut frankfurt = entry("frankfurt", "https://fra.example.net");
    frankfurt.additional_origins = vec![Origin::parse("https://transfer.example.net:8443").unwrap()];
    let mut catalog = ServerCatalog::singleton();
    catalog.servers.push(frankfurt);
    catalog
}

fn received(document: Value) -> Result<(ServerCatalog, Vec<Rejected>), serde_json::Error> {
    let received = ServerCatalog::decode(&serde_json::to_vec(&document).unwrap())?;
    Ok((received.catalog, received.rejected))
}

#[test]
fn the_singleton_catalogue_holds_self_alone() {
    let singleton = ServerCatalog::singleton();
    assert_eq!(singleton.validate(), Ok(()));
    let expected =
        json!({"defaultSelection": ["self"], "servers": [{"id": "self", "url": ".", "name": "graphite-meter"}]});
    assert_eq!(serde_json::to_value(&singleton).unwrap(), expected);
}

#[test]
fn a_catalogue_encodes_as_the_schema_spells_it_and_decodes_back() {
    let encoded = serde_json::to_value(catalog()).unwrap();
    let frankfurt = json!({
        "id": "frankfurt",
        "url": "https://fra.example.net",
        "name": "frankfurt",
        "additionalOrigins": ["https://transfer.example.net:8443"],
    });
    assert_eq!(encoded["servers"][1], frankfurt);
    assert_eq!(received(encoded).unwrap(), (catalog(), Vec::new()));
}

#[test]
fn a_published_catalogue_holds_self_first_and_at_most_32_servers() {
    let mut catalog = catalog();
    catalog.servers.swap(0, 1);
    assert_eq!(catalog.validate(), Err(CatalogError::Servers));
    let mut catalog = ServerCatalog::singleton();
    let others = (1..32).map(|index| entry(&format!("s{index}"), &format!("https://s{index}.example")));
    catalog.servers.extend(others);
    assert_eq!(catalog.validate(), Ok(()));
    catalog.servers.push(entry("s32", "https://s32.example"));
    assert_eq!(catalog.validate(), Err(CatalogError::Servers));
    catalog.servers.clear();
    assert_eq!(catalog.validate(), Err(CatalogError::Servers));
}

#[test]
fn published_entries_are_unique_and_bounded() {
    let (mut long_name, mut long_location) = (entry("a", "https://a.example"), entry("b", "https://b.example"));
    long_name.name = "n".repeat(257);
    long_location.location = "l".repeat(257);
    let faults = [
        (entry("frankfurt", "."), CatalogError::Origin),
        (entry("self", "https://other.example"), CatalogError::Duplicate),
        (entry("other", "HTTPS://FRA.example.net:443"), CatalogError::Duplicate),
        (long_name, CatalogError::Identity),
        (long_location, CatalogError::Identity),
    ];
    for (added, fault) in faults {
        let mut catalog = catalog();
        catalog.servers.push(added.clone());
        assert_eq!(catalog.validate(), Err(fault), "{added:?}");
    }
    let mut catalog = catalog();
    let origin = |index| Origin::parse(&format!("https://t{index}.example")).unwrap();
    catalog.servers[1].additional_origins = (0..32).map(origin).collect();
    assert_eq!(catalog.validate(), Ok(()));
    catalog.servers[1].additional_origins.push(origin(32));
    assert_eq!(catalog.validate(), Err(CatalogError::AdditionalOrigins));
}

#[test]
fn server_ids_are_one_to_64_safe_ascii_bytes() {
    for text in ["a", "A.b_c-9", &"x".repeat(64)] {
        assert_eq!(ServerId::parse(text).map(|id| id.to_string()).as_deref(), Some(text));
    }
    for text in ["", "a b", "ü", "a/b", &"x".repeat(65)] {
        assert_eq!(ServerId::parse(text), None, "{text:?}");
    }
}

#[test]
fn selections_hold_one_to_four_distinct_known_servers() {
    let mut catalog = catalog();
    catalog
        .servers
        .extend(["a", "b", "c"].map(|name| entry(name, &format!("https://{name}.example"))));
    let ids = |names: &[&str]| names.iter().map(|name| id(name)).collect::<Vec<_>>();
    assert_eq!(catalog.validate_selection(&ids(&["self"])), Ok(()));
    assert_eq!(catalog.validate_selection(&ids(&["self", "frankfurt", "a", "b"])), Ok(()));
    for selection in [&[][..], &["self", "frankfurt", "a", "b", "c"], &["a", "a"], &["unknown"]] {
        assert_eq!(catalog.validate_selection(&ids(selection)), Err(CatalogError::Selection), "{selection:?}");
    }
}

#[test]
fn a_received_entry_that_stays_invalid_is_left_out_alone() {
    let document = json!({
        "defaultSelection": ["broken", "frankfurt"],
        "servers": [
            {"id": "self", "url": ".", "name": "graphite-meter"},
            {"id": "frankfurt", "url": "https://fra.example.net/", "name": "Frankfurt"},
            {"id": "broken", "url": "https://user@broken.example", "name": "Broken"},
            {"id": "bad id", "url": "https://bad.example"},
            {"id": "twin", "url": "https://FRA.example.net"},
            {"id": "dot", "url": "."},
            {"id": "many", "url": "https://many.example", "additionalOrigins": vec!["https://x.example"; 33]},
            {"id": "extra", "url": "https://extra.example", "additionalOrigins": ["https://x.example/path"]},
        ],
    });
    let (catalog, rejected) = received(document).unwrap();
    let ids: Vec<_> = catalog.servers.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(ids, ["self", "frankfurt"]);
    let slash_dropped = BaseUrl::parse("https://fra.example.net").unwrap();
    assert_eq!(catalog.servers[1].url, slash_dropped, "one slash may end it");
    assert_eq!(catalog.default_selection, [id("frankfurt")]);
    let faults: Vec<_> = rejected.iter().map(|entry| (entry.id.as_str(), entry.error)).collect();
    assert_eq!(
        faults,
        [
            ("broken", CatalogError::Origin),
            ("bad id", CatalogError::Identity),
            ("twin", CatalogError::Duplicate),
            ("dot", CatalogError::Origin),
            ("many", CatalogError::AdditionalOrigins),
            ("extra", CatalogError::Origin),
        ]
    );
}

#[test]
fn a_received_catalogue_still_holds_as_a_whole() {
    let own = json!({"id": "self", "url": ".", "name": "graphite-meter"});
    let other = |index: usize| json!({"id": format!("s{index}"), "url": format!("https://s{index}.example")});
    let (fallback, _) = received(json!({"defaultSelection": ["gone"], "servers": [own]})).unwrap();
    assert_eq!(fallback.default_selection, [ServerId::own()]);
    let too_many: Vec<_> = std::iter::once(own.clone()).chain((1..33).map(other)).collect();
    for document in [
        json!({"defaultSelection": ["self"], "servers": []}),
        json!({"defaultSelection": ["self"], "servers": [other(1), own]}),
        json!({"defaultSelection": ["self"], "servers": [{"id": "self", "url": "https://a.example//"}]}),
        json!({"defaultSelection": ["self", "self"], "servers": [own]}),
        json!({"defaultSelection": ["self"]}),
        json!({"defaultSelection": ["self"], "servers": [own, {"id": 7}]}),
        json!({"defaultSelection": ["self"], "servers": too_many}),
    ] {
        assert!(received(document.clone()).is_err(), "{document}");
    }
}

#[test]
fn discovery_may_use_the_entry_host_on_any_port_or_an_exact_additional_origin() {
    let frankfurt = &catalog().servers[1];
    let served = Origin::parse("https://fra.example.net").unwrap();
    let approves = |targets: &[&str]| {
        let target = |base| json!({"baseUrl": base, "transport": "fetch-stream", "protocol": "http2"});
        let capabilities = json!({"throughput": targets.iter().map(target).collect::<Vec<_>>(), "latency": []});
        let document =
            json!({"server": {"name": ""}, "engineVersion": "", "generation": "g", "capabilities": capabilities});
        frankfurt.approves(&served, &Preflight::decode(&serde_json::to_vec(&document).unwrap()).unwrap())
    };
    assert!(approves(&[".", "https://FRA.example.net:7249", "http://fra.example.net:7246"]));
    assert!(approves(&["https://transfer.example.net:8443"]));
    assert!(!approves(&["https://transfer.example.net"]), "additional origins match exactly");
    assert!(!approves(&["https://sub.fra.example.net"]));
    assert!(!approves(&[".", "https://other.example"]));
}

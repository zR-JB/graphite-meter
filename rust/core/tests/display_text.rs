use graphite_meter_core::{
    catalog::{CatalogError, ServerCatalog},
    discovery::{DiscoveryError, Preflight},
    text,
};
use serde_json::{Value, json};

#[test]
fn catalog_and_preflight_reject_controlled_labels_without_rejecting_unicode() {
    let catalog = json!({
        "defaultSelection": ["self"],
        "servers": [{"id": "self", "url": ".", "name": "meter", "location": "here"}]
    });
    let preflight: Value = serde_json::from_slice(include_bytes!("../../../api/preflight.golden.json")).unwrap();
    let controls = ('\u{0}'..='\u{1f}')
        .chain('\u{7f}'..='\u{9f}')
        .chain(['\u{061c}', '\u{200e}', '\u{200f}'])
        .chain('\u{202a}'..='\u{202e}')
        .chain('\u{2066}'..='\u{2069}');
    let labels = controls
        .map(|c| (format!("before{c}after"), false))
        .chain([("München 東京 العربية e\u{301} 👩\u{200d}💻 می\u{200c}روم".into(), true)]);
    for (label, valid) in labels {
        for field in ["name", "location"] {
            let mut value = catalog.clone();
            value["servers"][0][field] = json!(label);
            let parsed: ServerCatalog = serde_json::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(
                parsed.validate(),
                if valid {
                    Ok(())
                } else {
                    Err(CatalogError::InvalidIdentity)
                },
                "catalog {field}: {label:?}"
            );
            if valid {
                assert_eq!(serde_json::to_value(parsed).unwrap()["servers"][0][field], label);
            }
        }
        for pointer in ["/server/name", "/server/location", "/engineVersion"] {
            let mut value = preflight.clone();
            *value.pointer_mut(pointer).unwrap() = json!(label);
            let parsed = Preflight::decode(&serde_json::to_vec(&value).unwrap());
            if valid {
                assert_eq!(
                    serde_json::to_value(parsed.unwrap()).unwrap().pointer(pointer).unwrap(),
                    &json!(label)
                );
            } else {
                assert_eq!(
                    parsed.unwrap_err(),
                    DiscoveryError::InvalidMetadata,
                    "preflight {pointer}: {label:?}"
                );
            }
        }
    }
}

#[test]
fn clean_text_matches_go_vectors() {
    // go/internal/wire/text_test.go; a Rust string cannot hold its invalid UTF-8 vector.
    for (input, limit, cleaned) in [
        ("Frankfurt · DE", 64, "Frankfurt · DE"),
        ("osc\x1b]52;c;cHduZWQ=\x07", 64, "osc ]52;c;cHduZWQ= "),
        ("c1\u{009b}2J", 64, "c1 2J"),
        ("del\x7f", 64, "del "),
        ("two\nlines", 64, "two lines"),
        ("evil\u{202e}gnp.exe", 64, "evil gnp.exe"),
        ("iso\u{2066}late\u{2069}", 64, "iso late "),
        ("arabic\u{061c}mark", 64, "arabic mark"),
        ("123456789", 5, "1234…"),
        ("12345", 5, "12345"),
        ("é", 0, "…"),
        ("", 0, ""),
    ] {
        assert_eq!(text::clean(input, limit), cleaned, "{input:?}");
    }
}

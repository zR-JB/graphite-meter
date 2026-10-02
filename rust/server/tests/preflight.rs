use graphite_meter_core::discovery::Protocol;
use graphite_meter_server::{
    config::Config,
    preflight::{Preflight, connect_origins},
};
use std::{collections::BTreeSet, sync::Arc};

#[test]
fn public_roles_merge_default_ports_and_csp_includes_socket_schemes() {
    let mut config = Config {
        advertised_native: Some(BTreeSet::new()),
        ..Config::default()
    };
    config.public.both = vec!["self".into(), "https://meter.example".into()];
    config.public.throughput = vec!["https://METER.example:443".into()];
    config.public.latency = vec!["https://meter.example:443".into()];
    let preflight = Preflight::new(Arc::new(config.validated().unwrap())).unwrap();
    let document = preflight.build("meter.example").unwrap();
    assert_eq!(document.capabilities.throughput.len(), 2);
    assert_eq!(document.capabilities.latency.len(), 2);
    assert!(
        document
            .capabilities
            .throughput
            .iter()
            .all(|target| target.protocol == Protocol::Negotiated)
    );
    assert_eq!(
        connect_origins(&document),
        ["https://meter.example", "wss://meter.example"]
    );
}

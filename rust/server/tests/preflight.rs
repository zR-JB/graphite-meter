use graphite_meter_core::discovery::{Protocol, ThroughputTarget};
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
    let capabilities = &document.capabilities;
    assert_eq!((capabilities.throughput.len(), capabilities.latency.len()), (2, 2));
    let negotiated = |target: &ThroughputTarget| target.protocol == Protocol::Negotiated;
    assert!(capabilities.throughput.iter().all(negotiated));
    let origins = connect_origins(&document);
    assert_eq!(origins, ["https://meter.example", "wss://meter.example"]);
}

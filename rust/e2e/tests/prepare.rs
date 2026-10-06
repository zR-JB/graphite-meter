//! Path checks against the server: each path.
use graphite_meter_client::{
    net::{Client, LatencyPath, ThroughputPath},
    run::prepare::{Paths, Prepared, prepare},
};
use graphite_meter_e2e::{self as e2e, Server};
use graphite_meter_net::Pool;
use graphite_meter_proto::{
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    origin::Origin,
};
use std::{sync::Arc, time::Duration};

async fn check(url: &Origin, args: &[&str]) -> Prepared {
    let config = e2e::config(&[&["-url", &url.to_string()], args].concat());
    let client = Client::new(config.insecure, Arc::new(Pool::inline()));
    prepare(&config, client).await.unwrap()
}

/// The paths of the one server `url` selects.
async fn paths(url: &Origin, args: &[&str]) -> Paths {
    let prepared = check(url, args).await;
    let [server] = prepared.servers.as_slice() else {
        panic!("one server: {:?}", prepared.servers);
    };
    server.path.clone().unwrap()
}

fn fetch(origin: &Origin, protocol: Protocol) -> ThroughputPath {
    ThroughputPath {
        origin: origin.clone(),
        transport: ThroughputTransport::FetchStream,
        protocol,
    }
}

fn latency(origin: &Origin, transport: LatencyTransport) -> Option<LatencyPath> {
    Some(LatencyPath { origin: origin.clone(), transport })
}

#[tokio::test]
async fn preparation_checks_each_path() {
    let server = Server::start().await;
    let (http2, http3) = (server.http2.to_string(), server.http3.to_string());
    let webtransport = ThroughputPath {
        origin: server.http3.clone(),
        transport: ThroughputTransport::WebTransport,
        protocol: Protocol::Http3,
    };
    let rows = [
        (
            vec![],
            fetch(&server.http1, Protocol::Http1),
            latency(&server.http3, LatencyTransport::WebTransport),
        ),
        (
            vec!["-throughput-origin", &http2, "-latency-transport", "websocket"],
            fetch(&server.http2, Protocol::Http2),
            latency(&server.http1, LatencyTransport::WebSocket),
        ),
        (
            vec!["-throughput-origin", &http3, "-stages", "down", "-loaded-latency=false"],
            fetch(&server.http3, Protocol::Http3),
            None,
        ),
        (
            vec!["-throughput-transport", "webtransport"],
            webtransport,
            latency(&server.http3, LatencyTransport::WebTransport),
        ),
    ];
    for (args, throughput, latency) in rows {
        let paths = paths(&server.http1, &[&["-insecure"], args.as_slice()].concat()).await;
        assert_eq!((&paths.throughput, &paths.latency), (&throughput, &latency), "{args:?}");
        assert_eq!(paths.latency.is_some(), paths.idle_rtt > Duration::ZERO, "{args:?}");
        assert_eq!(paths.stage_limit, Duration::from_secs(300));
    }
    let negotiated = Server::with(&[
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "none"),
        ("GM_PUBLIC_ORIGINS", "self"),
        ("GM_MAX_STAGE_DURATION", "90s"),
    ])
    .await;
    let paths = paths(&negotiated.http1, &[]).await;
    assert_eq!(
        paths.throughput,
        fetch(&negotiated.http1, Protocol::Http1),
        "a probe resolves the protocol"
    );
    assert_eq!(paths.latency, latency(&negotiated.http1, LatencyTransport::WebSocket));
    assert_eq!(paths.stage_limit, Duration::from_secs(90));
}

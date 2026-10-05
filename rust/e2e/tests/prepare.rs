//! Path checks against the server: each path, the fallbacks, failures by name and refused sign-ins.
use graphite_meter_client::{
    config::{self, Config, Parsed},
    net::{LatencyPath, ThroughputPath},
    run::prepare::{Paths, Prepared, prepare},
};
use graphite_meter_e2e::Server;
use graphite_meter_net::Pool;
use graphite_meter_proto::{
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    origin::Origin,
    reason::FailureReason,
};
use std::{
    ffi::OsString,
    sync::Arc,
    time::{Duration, Instant},
};

fn config(url: &Origin, args: &[&str]) -> Config {
    let url = url.to_string();
    let args = ["-url", &url]
        .into_iter()
        .chain(args.iter().copied())
        .map(OsString::from);
    match config::parse(args) {
        Ok(Parsed::Run(config)) => *config,
        other => panic!("{other:?}"),
    }
}

async fn check(url: &Origin, args: &[&str]) -> Prepared {
    prepare(&config(url, args), Arc::new(Pool::new().unwrap()))
        .await
        .unwrap()
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

#[tokio::test]
async fn without_quic_latency_falls_back_to_websocket_and_forced_webtransport_fails() {
    let server = Server::with(&[("GM_H3_PUBLIC_ORIGIN", "https://localhost:1")]).await;
    let started = Instant::now();
    let paths = paths(&server.http1, &["-insecure"]).await;
    assert_eq!(paths.throughput, fetch(&server.http1, Protocol::Http1));
    assert_eq!(paths.latency, latency(&server.http1, LatencyTransport::WebSocket));
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    for forced in [["-throughput-transport", "webtransport"], ["-latency-transport", "webtransport"]] {
        let prepared = check(&server.http1, &[&["-insecure"], forced.as_slice()].concat()).await;
        let failure = prepared.servers[0].path.clone().unwrap_err();
        assert_ne!(failure.reason, FailureReason::PreparationFailed, "{forced:?}: {failure:?}");
    }
}

#[tokio::test]
async fn an_unavailable_selected_server_fails_by_name_and_an_unselected_one_does_not_block() {
    let catalogue = r#"{"servers": [{"id": "gone", "url": "http://127.0.0.7:1", "name": "Gone"}]}"#;
    let server = Server::with(&[("GM_SERVER_CATALOG", catalogue)]).await;
    let prepared = check(&server.http1, &[]).await;
    let ids: Vec<_> = prepared.servers.iter().map(|server| server.id.as_str()).collect();
    assert_eq!(ids, ["self"]);
    assert!(prepared.servers[0].path.is_ok(), "{:?}", prepared.servers[0]);
    let prepared = check(&server.http1, &["-server", "gone", "-server", "self"]).await;
    let [own, gone] = prepared.servers.as_slice() else {
        panic!("two servers: {:?}", prepared.servers);
    };
    assert_eq!((own.name.as_str(), gone.name.as_str()), ("graphite-meter", "Gone"));
    assert!(own.path.is_ok(), "{own:?}");
    let failure = gone.path.clone().unwrap_err();
    assert_eq!(
        (failure.reason, failure.text.as_str()),
        (FailureReason::ConnectionLost, "Server could not be reached")
    );
}

#[tokio::test]
async fn a_protected_server_is_refused_over_http_and_with_insecure_tls() {
    const HASH: &str =
        "$argon2id$v=19$m=19456,t=2,p=1$OT2po7nOdP+21BKX5CuZQw$9kVgfSWvlFy31939zUCVY62fHIuSqC8RwL67EpQ8qy8";
    let server = Server::with(&[
        ("GM_AUTH_MODE", "password"),
        ("GM_AUTH_PUBLIC_URL", "https://127.0.0.7"),
        ("GM_AUTH_PASSWORD_HASH", HASH),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "http2,http3"),
    ])
    .await;
    let refusals: [(&Origin, &[&str], &str); 2] = [
        (&server.http1, &[], "authenticated operation requires an HTTPS -url"),
        (
            &server.http2,
            &["-insecure"],
            "sign-in refuses skipped TLS verification (Skip TLS verify, -insecure)",
        ),
    ];
    for (url, args, refusal) in refusals {
        let Err(failure) = prepare(&config(url, args), Arc::new(Pool::new().unwrap())).await else {
            panic!("{url} was prepared");
        };
        assert_eq!((failure.reason, failure.text.as_str()), (FailureReason::PreparationFailed, refusal));
    }
}

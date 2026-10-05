//! Whole runs against the server: every stage over each transport, and a server that departs mid-stage.
use graphite_meter_client::{
    config::{self, Config, Parsed},
    events::{Event, Events},
    model::{Direction, Outcome, Scope, Stage, StageResult},
    net::Client,
    run::{coordinator, prepare::prepare},
};
use graphite_meter_e2e::Server;
use graphite_meter_net::Pool;
use graphite_meter_proto::{catalog::ServerId, origin::Origin, reason::FailureReason};
use graphite_meter_testkit::{Fault, Link};
use std::{ffi::OsString, net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Every stage for 2 s.
const SHORT: [&str; 11] = [
    "-stages",
    "latency,download,upload,bidirectional",
    "-latency-duration",
    "2s",
    "-download-duration",
    "2s",
    "-upload-duration",
    "2s",
    "-bidirectional-duration",
    "2s",
    "-insecure",
];

/// `args` at `url` without warmup, two HTTP/1.1 lanes a direction.
fn config(url: &Origin, args: &[&str]) -> Config {
    let url = url.to_string();
    let args = [&["-url", &url, "-warmup", "0s", "-auto-streams", "2"], args].concat();
    match config::parse(args.into_iter().map(OsString::from)) {
        Ok(Parsed::Run(config)) => *config,
        other => panic!("{other:?}"),
    }
}

/// Prepares and runs `config`, handing each event to `seen` as it arrives.
async fn run(config: &Config, seen: impl AsyncFnMut(&Event)) -> (Outcome, Vec<Event>) {
    let client = Client::new(config.insecure, Arc::new(Pool::new().unwrap()));
    let prepared = prepare(config, client).await.unwrap();
    let (events, received) = Events::channel();
    let watched = watch(received, seen);
    let outcome = async {
        let outcome = coordinator::run(&prepared, config, &events, CancellationToken::new()).await;
        drop(events);
        outcome
    };
    tokio::join!(outcome, watched)
}

async fn watch(mut received: mpsc::UnboundedReceiver<Event>, mut seen: impl AsyncFnMut(&Event)) -> Vec<Event> {
    let mut events = Vec::new();
    while let Some(event) = received.recv().await {
        seen(&event).await;
        events.push(event);
    }
    events
}

/// A server that advertises the origin it is asked at, behind a link to its HTTP/1.1 listener.
async fn relayed() -> (Server, Link) {
    let server = Server::with(&[("GM_ADVERTISED_NATIVE_ENDPOINTS", "none"), ("GM_PUBLIC_ORIGINS", "self")]).await;
    let address: SocketAddr = server.http1.to_string().trim_start_matches("http://").parse().unwrap();
    (server, Link::tcp(address, Duration::ZERO).await.unwrap())
}

fn results(events: &[Event]) -> Vec<&StageResult> {
    let finished = events.iter().filter_map(|event| match event {
        Event::StageFinished(result) => Some(result),
        _ => None,
    });
    finished.collect()
}

async fn completes(server: &Server, args: &[&str]) {
    let args = [SHORT.as_slice(), args].concat();
    let (outcome, events) = run(&config(&server.http1, &args), async |_| {}).await;
    let results = results(&events);
    assert_eq!(outcome, Outcome::Complete, "{args:?}: {results:#?}");
    let stages: Vec<_> = results.iter().map(|result| result.stage).collect();
    assert_eq!(stages, Stage::ALL);
    for result in results {
        let [server] = result.servers.as_slice() else {
            panic!("one server: {result:?}")
        };
        let latency = server.latency.unwrap().summary;
        assert!(latency.replies > 0 && latency.p50.is_some(), "{:?}: {latency:?}", result.stage);
        for &direction in result.stage.directions() {
            let throughput = result.throughput[direction].unwrap();
            assert!(throughput.rate.is_some() && throughput.bytes > 0, "{:?} {direction:?}", result.stage);
        }
    }
    let measuring = events.iter().filter_map(|event| match event {
        Event::Measuring(stage) => Some(*stage),
        _ => None,
    });
    assert_eq!(measuring.collect::<Vec<_>>(), Stage::ALL);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Probe { rtt: Some(_), .. }))
    );
    let up = |event: &Event| matches!(event, Event::Sample { rates, .. } if rates.up.is_some_and(|rate| rate > 0.0));
    assert!(events.iter().any(up), "live upload rates");
}

#[tokio::test]
async fn a_run_completes_over_http1_with_websocket_latency() {
    completes(&Server::start().await, &["-latency-transport", "websocket"]).await;
}

#[tokio::test]
async fn a_run_completes_over_http2_with_websocket_latency() {
    let server = Server::start().await;
    let origin = server.http2.to_string();
    completes(&server, &["-throughput-origin", &origin, "-latency-transport", "websocket"]).await;
}

#[tokio::test]
async fn a_run_completes_over_http3_with_datagram_latency() {
    let server = Server::start().await;
    let origin = server.http3.to_string();
    completes(&server, &["-throughput-origin", &origin, "-latency-transport", "webtransport"]).await;
}

#[tokio::test]
async fn a_run_completes_over_webtransport_with_datagram_latency() {
    let args = ["-throughput-transport", "webtransport", "-latency-transport", "webtransport"];
    completes(&Server::start().await, &args).await;
}

#[tokio::test]
async fn a_server_stalled_mid_download_departs_and_the_other_completes_partial() {
    let (_relayed, link) = relayed().await;
    let catalogue = format!(
        r#"{{"servers": [{{"id": "relayed", "url": "http://{}", "name": "Relayed"}}]}}"#,
        link.address
    );
    let server = Server::with(&[("GM_SERVER_CATALOG", &catalogue)]).await;
    let args = ["-server", "self", "-server", "relayed", "-stages", "download", "-download-duration", "6s"];
    let config = config(&server.http1, &[&["-insecure"], args.as_slice()].concat());
    let stall = async |event: &Event| {
        if matches!(event, Event::Measuring(_)) {
            tokio::time::sleep(Duration::from_secs(1)).await;
            link.inject(Fault::Stall);
        }
    };
    let (outcome, events) = run(&config, stall).await;
    let [result] = results(&events)[..] else { panic!("one stage: {events:?}") };
    assert_eq!(outcome, Outcome::Partial, "{result:#?}");
    let relayed = ServerId::parse("relayed").unwrap();
    let left: Vec<_> = result
        .servers
        .iter()
        .filter_map(|server| server.left.then_some(&server.server))
        .collect();
    assert_eq!(left, [&relayed]);
    let failure = &result.failures[0];
    assert_eq!(
        (&failure.server, failure.scope, failure.failure.reason),
        (&relayed, Scope::Throughput, FailureReason::Timeout)
    );
    assert!(result.throughput[Direction::Down].unwrap().rate.is_some());
    let announced = |event: &Event| matches!(event, Event::ServerFailed { server, .. } if *server == relayed);
    assert!(events.iter().any(announced));
}

#[tokio::test]
async fn a_sole_server_that_fails_a_stage_rejoins_the_next() {
    let (_server, link) = relayed().await;
    let url = Origin::parse(&format!("http://{}", link.address)).unwrap();
    let args = ["-stages", "down,up", "-download-duration", "4s", "-upload-duration", "2s"];
    let mut stalled = false;
    let stall = async |event: &Event| match event {
        Event::Measuring(_) if !stalled => {
            stalled = true;
            tokio::time::sleep(Duration::from_millis(1500)).await;
            link.inject(Fault::Stall);
        }
        Event::StageFinished(_) => link.inject(Fault::None),
        _ => {}
    };
    let (outcome, events) = run(&config(&url, &args), stall).await;
    let [download, upload] = results(&events)[..] else {
        panic!("two stages: {events:?}")
    };
    assert_eq!(outcome, Outcome::Partial, "{download:#?}");
    let failure = &download.failures[0];
    assert_eq!((download.servers[0].left, failure.failure.reason), (true, FailureReason::Timeout));
    assert!(!upload.servers[0].left && upload.failures.is_empty(), "{upload:#?}");
    assert!(upload.throughput[Direction::Up].unwrap().rate.is_some());
}

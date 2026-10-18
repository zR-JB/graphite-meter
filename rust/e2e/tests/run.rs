//! Whole runs against the server on pinned runtimes: every stage over each transport and a server that departs
//! mid-stage.
use graphite_meter_client::{
    config::Config,
    events::{Event, Events},
    model::{Direction, Outcome, Scope, StageResult},
    net::Client,
    run::{coordinator, prepare::prepare},
};
use graphite_meter_e2e::{self as e2e, Server};
use graphite_meter_net::Pool;
use graphite_meter_proto::{catalog::ServerId, origin::Origin, reason::FailureReason};
use graphite_meter_testkit::Link;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{runtime::Handle, sync::mpsc};
use tokio_util::sync::CancellationToken;

/// Short stages; transfers get 2 s so a slow lane start on a loaded runner still leaves 800 ms of evidence.
const SHORT: [&str; 11] = [
    "-stages",
    "latency,download,upload,bidirectional",
    "-latency-duration",
    "1s",
    "-download-duration",
    "2s",
    "-upload-duration",
    "2s",
    "-bidirectional-duration",
    "2s",
    "-insecure",
];

/// The stages whose lanes differ by transport; the HTTP/1.1 run covers the rest once.
const TRANSFERS: &str = "upload,bidirectional";

/// `args` at `url` without warmup, two HTTP/1.1 lanes a direction.
fn config(url: &Origin, args: &[&str]) -> Config {
    let url = url.to_string();
    e2e::config(&[&["-url", &url, "-warmup", "0s", "-auto-streams", "2"], args].concat())
}

/// The pinned runtimes and the latency runtime beside the test's multi-thread runtime, as the binary runs them.
fn pinned() -> Arc<Pool> {
    Arc::new(Pool::probing(&Handle::current()).unwrap())
}

/// Prepares and runs `config` on pinned runtimes within 30 s, handing each event to `seen` as it arrives.
async fn run(config: &Config, seen: impl AsyncFnMut(&Event)) -> (Outcome, Vec<Event>) {
    let client = Client::new(config.insecure, pinned());
    let prepared = prepare(config, client).await.unwrap();
    let (events, received) = Events::channel();
    let watched = watch(received, seen);
    let outcome = async {
        let outcome = coordinator::run(&prepared, config, &events, CancellationToken::new()).await;
        drop(events);
        outcome
    };
    let ran = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(outcome, watched)
    });
    ran.await.expect("the run ends")
}

async fn watch(
    mut received: mpsc::UnboundedReceiver<Event>,
    mut seen: impl AsyncFnMut(&Event),
) -> Vec<Event> {
    let mut events = Vec::new();
    while let Some(event) = received.recv().await {
        seen(&event).await;
        events.push(event);
    }
    events
}

/// A server that advertises the origin it is asked at, behind a link to its HTTP/1.1 listener.
async fn relayed() -> (Server, Link) {
    let server = Server::with(&[
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "none"),
        ("GM_PUBLIC_ORIGINS", "self"),
    ])
    .await;
    let address: SocketAddr = server
        .http1
        .to_string()
        .trim_start_matches("http://")
        .parse()
        .unwrap();
    (server, Link::tcp(address, Duration::ZERO).await.unwrap())
}

fn results(events: &[Event]) -> Vec<&StageResult> {
    let finished = events.iter().filter_map(|event| match event {
        Event::StageFinished(result) => Some(result),
        _ => None,
    });
    finished.collect()
}

/// A run of every stage, or of the `-stages` in `args`, completes with what each stage measures.
async fn completes(server: &Server, args: &[&str]) {
    let args = [SHORT.as_slice(), args].concat();
    let config = config(&server.http1, &args);
    let (outcome, events) = run(&config, async |_| {}).await;
    let results = results(&events);
    assert_eq!(outcome, Outcome::Complete, "{args:?}: {results:#?}");
    let stages: Vec<_> = results.iter().map(|result| result.stage).collect();
    assert_eq!(stages, config.stages);
    for result in results {
        let [server] = result.servers.as_slice() else {
            panic!("one server: {result:?}")
        };
        let latency = server.latency.unwrap().summary;
        assert!(
            latency.replies > 0 && latency.p50.is_some(),
            "{:?}: {latency:?}",
            result.stage
        );
        for &direction in result.stage.directions() {
            let throughput = result.throughput[direction].unwrap();
            assert!(
                throughput.rate.is_some() && throughput.bytes > 0,
                "{:?} {direction:?}",
                result.stage
            );
        }
    }
    let measuring = events.iter().filter_map(|event| match event {
        Event::Measuring(stage) => Some(*stage),
        _ => None,
    });
    assert_eq!(measuring.collect::<Vec<_>>(), config.stages);
    let replied = |event: &Event| match event {
        Event::Probes {
            probes, latency, ..
        } => probes.iter().any(|probe| probe.rtt.is_some()) && latency.replies > 0,
        _ => false,
    };
    assert!(
        events.iter().any(replied),
        "live round trips and the latency they add up to"
    );
    let up = |event: &Event| matches!(event, Event::Sample { rates, .. } if rates.up.is_some_and(|rate| rate > 0.0));
    assert!(events.iter().any(up), "live upload rates");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_completes_over_http1_with_websocket_latency() {
    completes(&Server::start().await, &["-latency-transport", "websocket"]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_completes_over_http2_with_websocket_latency() {
    let server = Server::start().await;
    let origin = server.http2.to_string();
    let args = [
        "-throughput-origin",
        &origin,
        "-latency-transport",
        "websocket",
        "-stages",
        TRANSFERS,
    ];
    completes(&server, &args).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_completes_over_http3_with_datagram_latency() {
    let server = Server::start().await;
    let origin = server.http3.to_string();
    let args = [
        "-throughput-origin",
        &origin,
        "-latency-transport",
        "webtransport",
        "-stages",
        TRANSFERS,
    ];
    completes(&server, &args).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_completes_over_webtransport_with_datagram_latency() {
    let args = [
        "-throughput-transport",
        "webtransport",
        "-latency-transport",
        "webtransport",
        "-stages",
        TRANSFERS,
    ];
    completes(&Server::start().await, &args).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_stalled_mid_download_departs_and_the_other_completes_partial() {
    let (_relayed, link) = relayed().await;
    let catalog = format!(
        r#"{{"servers": [{{"id": "relayed", "url": "http://{}", "name": "Relayed"}}]}}"#,
        link.address
    );
    let server = Server::with(&[("GM_SERVER_CATALOG", &catalog)]).await;
    let args = [
        "-server",
        "self",
        "-server",
        "relayed",
        "-stages",
        "download",
        "-download-duration",
        "4s",
    ];
    let config = config(&server.http1, &[&["-insecure"], args.as_slice()].concat());
    let stall = async |event: &Event| {
        if matches!(event, Event::Sample { rates, .. } if rates.down.is_some_and(|rate| rate > 0.0))
        {
            link.stall();
        }
    };
    let (outcome, events) = run(&config, stall).await;
    let [result] = results(&events)[..] else {
        panic!("one stage: {events:?}")
    };
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
    let announced =
        |event: &Event| matches!(event, Event::ServerFailed(failure) if failure.server == relayed);
    assert!(events.iter().any(announced));
}

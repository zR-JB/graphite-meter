//! Lane groups and QUIC dials against the server.
use graphite_meter_client::{
    model::{Dir, LaneHealth, Stage},
    net::{Client, Fault, Lanes, Request, ThroughputPath, Work, topology},
};
use graphite_meter_e2e::Server;
use graphite_meter_net::{ConnectError, Pool};
use graphite_meter_proto::{
    discovery::{Probe, Protocol, ThroughputTransport},
    json,
    origin::Origin,
    route::Route,
    upload::{Counters, Session},
};
use graphite_meter_testkit::{self as testkit, Link};
use http::Method;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

const MIB: u64 = 1 << 20;

fn client() -> Client {
    Client::new(true, Arc::new(Pool::inline()))
}

/// Waits up to `bound` for `done`, failing at once when a lane's fault stands.
async fn until(lanes: &mut Lanes, bound: Duration, mut done: impl AsyncFnMut(&Lanes) -> bool) {
    let deadline = Instant::now() + bound;
    while !done(lanes).await {
        if let LaneHealth::Failed(failure) = lanes.health() {
            panic!("a lane failed: {failure}");
        }
        assert!(Instant::now() < deadline, "lanes did not get there within {bound:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!matches!(lanes.health(), LaneHealth::Failed(_)), "{:?}", lanes.health());
}

async fn received(client: &Client, via: Protocol, origin: &Origin, id: &str) -> u64 {
    let mut request = Request::new(Method::POST, origin, Route::UploadCheckpoint);
    request.query.push(("id", id.to_owned()));
    let counters = client.json(via, request, json::decode::<Counters>).await;
    counters.map_or(0, Counters::bytes)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bidirectional_lanes_move_bytes_over_each_transport_on_pinned_runtimes() {
    let pinned = Arc::new(Pool::beside(&Handle::current()).unwrap());
    let (server, client) = (Server::start().await, Client::new(true, pinned.clone()));
    for (transport, protocol, origin) in [
        (ThroughputTransport::FetchStream, Protocol::Http1, &server.http1),
        (ThroughputTransport::FetchStream, Protocol::Http2, &server.http2),
        (ThroughputTransport::FetchStream, Protocol::Http3, &server.http3),
        (ThroughputTransport::WebTransport, Protocol::Http3, &server.http3),
    ] {
        let path = ThroughputPath { origin: origin.clone(), transport, protocol };
        let plans = topology(&path, Stage::Bidirectional, Dir { down: 2, up: 2 });
        let minted = Request::new(Method::POST, origin, Route::UploadSession);
        let id = client.json(protocol, minted, Session::decode).await.unwrap().upload_id;
        let token = CancellationToken::new();
        let mut download = Lanes::start(&client, plans.clone(), Work::Download, Duration::ZERO, token.clone());
        let mut upload = Lanes::start(&client, plans, Work::Upload(id.clone()), Duration::ZERO, token.clone());
        let bound = Duration::from_secs(20);
        until(&mut download, bound, async |lanes| lanes.ready() && lanes.bytes() >= MIB).await;
        let counted = async |lanes: &Lanes| lanes.ready() && received(&client, protocol, origin, &id).await >= MIB;
        until(&mut upload, bound, counted).await;
        assert_eq!(upload.bytes(), 0, "{transport:?} {protocol:?}: the receiver counts uploads");
        let tasks: usize = pinned
            .runtimes()
            .iter()
            .map(|runtime| runtime.metrics().num_alive_tasks())
            .sum();
        assert!(tasks >= 4, "{transport:?} {protocol:?}: {tasks} tasks on the pinned runtimes, four lanes");
        token.cancel();
    }
}

#[tokio::test]
async fn a_webtransport_download_counts_each_stream_s_payload_exactly() {
    let (server, client) = (Server::start().await, client());
    let path = ThroughputPath {
        origin: server.http3.clone(),
        transport: ThroughputTransport::WebTransport,
        protocol: Protocol::Http3,
    };
    let plans = topology(&path, Stage::Download, Dir { down: 1, up: 0 });
    let mut download = Lanes::start(&client, plans, Work::Download, Duration::ZERO, CancellationToken::new());
    // A lane fails on a 64 MiB stream that counts more or less than its payload, so it never gets past it.
    until(&mut download, Duration::from_secs(90), async |lanes| lanes.bytes() > 65 * MIB).await;
}

#[tokio::test]
async fn fourteen_webtransport_lanes_outgrow_the_first_connection_window_without_deadlock() {
    let (server, client) = (Server::start().await, client());
    let path = ThroughputPath {
        origin: server.http3.clone(),
        transport: ThroughputTransport::WebTransport,
        protocol: Protocol::Http3,
    };
    let plans = topology(&path, Stage::Download, Dir { down: 14, up: 0 });
    let mut download = Lanes::start(&client, plans, Work::Download, Duration::ZERO, CancellationToken::new());
    // Fourteen streams share one session's connection, whose receive window starts at 768 KiB.
    until(&mut download, Duration::from_secs(30), async |lanes| {
        lanes.ready() && lanes.bytes() > 16 * MIB
    })
    .await;
}

#[tokio::test]
async fn a_silent_quic_address_fails_after_3_s_while_a_delayed_answering_one_finishes() {
    let (server, client) = (Server::start().await, client());
    let silent = Link::udp(server.quic, Duration::ZERO).await.unwrap();
    silent.inject(testkit::Fault::Stall);
    let delayed = Link::udp(server.quic, Duration::from_secs(1)).await.unwrap();
    let timed = async |link: &Link| {
        let (started, origin) = (Instant::now(), Origin::parse(&format!("https://{}", link.address)).unwrap());
        let request = Request::new(Method::GET, &origin, Route::Probe);
        (client.json(Protocol::Http3, request, Probe::decode).await, started.elapsed())
    };
    let ((unanswered, silence), (answered, delay)) = tokio::join!(timed(&silent), timed(&delayed));
    assert!(matches!(unanswered, Err(Fault::Connect(ConnectError::Unreachable(_)))), "{unanswered:?}");
    assert!((3.0..4.0).contains(&silence.as_secs_f64()), "{silence:?}");
    answered.unwrap();
    assert!(delay > Duration::from_secs(3), "{delay:?} outlasts the silent address's bound");
}

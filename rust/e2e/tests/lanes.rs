//! Lane groups and latency buses against the server.
use graphite_meter_client::{
    model::{Dir, LaneHealth, Stage},
    net::{Class, Client, Fault, Lanes, LatencyPath, Request, ThroughputPath, Work, topology},
};
use graphite_meter_e2e::Server;
use graphite_meter_net::Pool;
use graphite_meter_proto::{
    bus::Ping,
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    json,
    lane::LaneEnding,
    origin::Origin,
    route::Route,
    upload::{Counters, Session},
};
use http::Method;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
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

#[tokio::test]
async fn bidirectional_lanes_move_bytes_over_each_transport() {
    let (server, client) = (Server::start().await, client());
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
async fn buses_carry_pings_over_websocket_and_webtransport_datagrams() {
    let (server, client) = (Server::start().await, client());
    for (origin, transport) in [
        (&server.http1, LatencyTransport::WebSocket),
        (&server.http3, LatencyTransport::WebTransport),
    ] {
        let mut bus = client
            .bus(&LatencyPath { origin: origin.clone(), transport })
            .await
            .unwrap();
        let reply = async {
            loop {
                bus.send(Ping { id: 7 }).await.unwrap();
                if let Ok(pong) = tokio::time::timeout(Duration::from_millis(500), bus.next()).await {
                    break pong.unwrap();
                }
            }
        };
        assert_eq!(tokio::time::timeout(Duration::from_secs(5), reply).await.unwrap().id, 7, "{transport:?}");
    }
}

#[tokio::test]
async fn a_websocket_bus_the_server_ends_as_idle_redials() {
    let (server, client) = (Server::start().await, client());
    let path = LatencyPath {
        origin: server.http1.clone(),
        transport: LatencyTransport::WebSocket,
    };
    let mut bus = client.bus(&path).await.unwrap();
    bus.send(Ping { id: 0 }).await.unwrap();
    assert_eq!(bus.next().await.unwrap().id, 0);
    let quiet = Instant::now();
    let ended = tokio::time::timeout(Duration::from_secs(60), bus.next()).await.unwrap();
    assert!(quiet.elapsed() >= Duration::from_secs(29), "{:?}", quiet.elapsed());
    let fault = ended.unwrap_err();
    assert!(matches!(fault, Fault::Ended(LaneEnding::Idle)), "{fault:?}");
    assert_eq!(fault.class(), Class::Redial);
    let mut redialled = client.bus(&path).await.unwrap();
    redialled.send(Ping { id: 1 }).await.unwrap();
    assert_eq!(redialled.next().await.unwrap().id, 1);
}

use super::*;
use crate::transport::Transport;
use crate::{
    model::{ServerLatency, StageStatus},
    net::Http,
};
use graphite_meter_core::discovery::{LatencyTarget, LatencyTransport, Protocol};
use graphite_meter_core::{catalog::ServerEntry, discovery::ThroughputTarget};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Barrier, Notify},
};

impl ServerContribution {
    fn down_bps(&self) -> Option<f64> {
        self.down.as_ref()?.mean_bytes_per_sec
    }
    fn up_bps(&self) -> Option<f64> {
        self.up.as_ref()?.mean_bytes_per_sec
    }
    fn down_bytes(&self) -> u64 {
        self.down.as_ref().map_or(0, |result| result.total_bytes)
    }
}

/// TLS fault peers retain scenarios the product-server interoperability replay cannot inject.
struct Fixture {
    config: Config,
    servers: Vec<PreparedServer>,
    modes: Vec<Arc<AtomicU8>>,
    started: Vec<Arc<Notify>>,
    /// Dropped with the fixture, which aborts every peer.
    peers: JoinSet<()>,
    snapshots: watch::Sender<Snapshot>,
    stop: watch::Sender<bool>,
}
impl Fixture {
    async fn new(ids: &[&str], millis: u64) -> Result<Self, Error> {
        Self::with_gates(ids, millis, &[]).await
    }

    async fn with_gates(ids: &[&str], millis: u64, gates: &[Option<Arc<Barrier>>]) -> Result<Self, Error> {
        let _ = crate::crypto::provider().install_default();
        let http = Http::new(true)?;
        let mut fixture = Self {
            // One lane without warmup or loaded latency unless the scenario overrides it.
            config: Config {
                latency_duration: Duration::from_millis(millis),
                download_duration: Duration::from_millis(millis),
                upload_duration: Duration::from_millis(millis),
                bidirectional_duration: Duration::from_millis(millis),
                insecure: true,
                warmup: Duration::ZERO,
                streams: 1,
                loaded_latency: false,
                ..Config::default()
            },
            servers: Vec::new(),
            modes: Vec::new(),
            started: Vec::new(),
            peers: JoinSet::new(),
            snapshots: watch::channel(Snapshot::default()).0,
            stop: watch::channel(false).0,
        };
        fixture.peers.spawn(heartbeat());
        for (index, id) in ids.iter().enumerate() {
            let gate = gates.get(index).cloned().flatten();
            let (origin, mode, started) = download_peer(gate, &mut fixture.peers).await?;
            fixture.modes.push(mode);
            fixture.started.push(started);
            fixture.servers.push(prepared_download(id, &origin, &http).await?);
        }
        fixture.config.url = fixture.servers[0].entry.url.clone();
        fixture.config.servers = ids.iter().map(|id| (*id).into()).collect();
        Ok(fixture)
    }

    fn latency(&mut self) {
        for server in &mut self.servers {
            let base_url = server.entry.url.clone();
            server.latency = Some(LatencyTarget { base_url, transport: LatencyTransport::WebSocket });
        }
    }

    async fn measure(&self, stage: Stage) -> Result<Vec<String>, Error> {
        let (config, servers, stop) = (&self.config, &self.servers, self.stop.subscribe());
        measure(stage, config, servers, &self.snapshots, stop, &mut RunLedger::new()).await
    }

    /// The coordinator consumes the prepared members; this fixture still owns their peers and publication.
    async fn run(&mut self) -> Result<(), Error> {
        let prepared = super::super::PreparedRun {
            servers: std::mem::take(&mut self.servers),
            key: self.config.preparation_key(),
            verified_at: Instant::now(),
        };
        let (config, http) = (self.config.clone(), Http::new(self.config.insecure)?);
        super::super::run(config, http, self.snapshots.clone(), self.stop.subscribe(), Some(prepared)).await
    }

    async fn phase(&self, phase: Phase) {
        let reached = |snapshot: &Snapshot| snapshot.phase == phase;
        self.snapshots.subscribe().wait_for(reached).await.unwrap();
    }

    /// Change one peer after the measurement window opens, retaining the outer fixture timeout.
    async fn fault(
        &self,
        stage: Stage,
        after: Duration,
        member: usize,
        mode: u8,
    ) -> Result<Result<Vec<String>, Error>, Error> {
        let inject = async {
            self.phase(Phase::Measuring).await;
            if !after.is_zero() {
                tokio::time::sleep(after).await;
            }
            self.modes[member].store(mode, Ordering::SeqCst);
        };
        Ok(joined(self.measure(stage), inject).await?.0)
    }
}

/// A TLS peer in `peers` whose mode selects its fault; with `gate`, its first transfer request waits there.
async fn download_peer(
    gate: Option<Arc<Barrier>>,
    peers: &mut JoinSet<()>,
) -> Result<(String, Arc<AtomicU8>, Arc<Notify>), Error> {
    let _ = crate::crypto::provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("https://{}", listener.local_addr()?);
    let tls = crate::fixtures::server_tls(&[&rustls::version::TLS13], &[])?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let failed = Arc::new(AtomicU8::new(0));
    let flag = failed.clone();
    let started = Arc::new(Notify::new());
    let starting = started.clone();
    let first_request = Arc::new(AtomicBool::new(false));
    let checkpoints = Arc::new(AtomicU64::new(0));
    // The receiver's count as its progress feed reports it.
    let progress = watch::channel(1_u64).0;
    let finalized = watch::channel(false).0;
    let mints = Arc::new(AtomicU64::new(0));
    // Tokio's clock, so a receiver on paused time counts the stage's time.
    let receiver_clock = Instant::now();
    let login_url = format!("{origin}/login");
    peers.spawn(async move {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break; };
                    let flag = flag.clone();
                    let acceptor = acceptor.clone();
                    let gate = gate.clone();
                    let first_request = first_request.clone();
                    let checkpoints = checkpoints.clone();
                    let progress = progress.clone();
                    let starting = starting.clone();
                    let finalized = finalized.clone();
                    let mints = mints.clone();
                    let login_url = login_url.clone();
                    clients.spawn(async move {
                        let Ok(mut stream) = acceptor.accept(stream).await else { return; };
                        let mut request = [0_u8; 4096];
                        let Ok(length) = stream.read(&mut request).await else { return; };
                        if request[..length].starts_with(b"GET /ws/ping ") {
                            use futures_util::SinkExt;
                            let Ok(header) = std::str::from_utf8(&request[..length]) else { return; };
                            let Some(key) = header.lines().find_map(|line| line.split_once(':').filter(|(name, _)| name.eq_ignore_ascii_case("sec-websocket-key")).map(|(_, value)| value.trim())) else { return; };
                            let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
                            if stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.is_err() { return; }
                            let mut socket = tokio_tungstenite::WebSocketStream::from_raw_socket(stream, tokio_tungstenite::tungstenite::protocol::Role::Server, None).await;
                            while let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) = socket.next().await {
                                let Ok(id) = graphite_meter_core::wire::decode_ping(&text) else { return; };
                                if flag.load(Ordering::SeqCst) == 8 {
                                    continue;
                                }
                                // Mode 6 ends the channel as idle, 9 as revoked.
                                let ending = match flag.load(Ordering::SeqCst) { 6 => Some(graphite_meter_core::failure::LaneEnding::Idle), 9 => Some(graphite_meter_core::failure::LaneEnding::Revoked), _ => None };
                                if let Some(ending) = ending {
                                    let _ = socket.send(tokio_tungstenite::tungstenite::Message::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                                        code: ending.websocket_code().into(),
                                        reason: ending.reason().into(),
                                    }))).await;
                                    return;
                                }
                                let reply = graphite_meter_core::wire::encode_pong(id, 0);
                                if socket.send(tokio_tungstenite::tungstenite::Message::Text(reply.into())).await.is_err() { return; }
                            }
                            return;
                        }
                        let request = &request[..length];
                        if request.starts_with(b"POST /upload/session") {
                            finalized.send_replace(false);
                            // Mode 20 forgets the upload id until the next mint, mode 21 for good.
                            let _ = flag.compare_exchange(20, 0, Ordering::SeqCst, Ordering::SeqCst);
                            let body = format!(r#"{{"uploadId":"test-session-{}"}}"#, mints.fetch_add(1, Ordering::SeqCst));
                            let _ = stream.write_all(crate::fixtures::ok(&body).as_bytes()).await;
                            return;
                        }
                        if request.starts_with(b"GET /upload/progress") {
                            if stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 68719476736\r\n\r\n{\"type\":\"ready\"}\n").await.is_err()
                                || stream.flush().await.is_err() { return; }
                            let mut finished = finalized.subscribe();
                            if flag.compare_exchange(11, 12, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                                let _ = finished.wait_for(|finished| *finished).await;
                            }
                            let mut observed = progress.subscribe();
                            let record = |kind: &str, bytes| format!("{{\"type\":\"{kind}\",\"bytes\":{bytes},\"nanos\":1}}\n");
                            while !*finished.borrow() {
                                let bytes = *observed.borrow_and_update();
                                // TLS may accept plaintext before its ciphertext reaches the socket.
                                // Flush before waiting for an event that depends on the client's readiness.
                                if stream.write_all(record("progress", bytes).as_bytes()).await.is_err()
                                    || stream.flush().await.is_err() { return; }
                                tokio::select! {
                                    _ = finished.changed() => {},
                                    _ = observed.changed() => {},
                                }
                            }
                            let complete = record("complete", *observed.borrow());
                            let _ = stream.write_all(complete.as_bytes()).await;
                            let _ = stream.flush().await;
                            return;
                        }
                        if request.starts_with(b"DELETE /upload/progress") {
                            finalized.send_replace(true);
                            let _ = flag.compare_exchange(12, 13, Ordering::SeqCst, Ordering::SeqCst);
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
                            return;
                        }
                        if request.starts_with(b"POST /upload?") {
                            let mut refused = false;
                            // Paced like the downloads, so paused time can pass while lanes upload.
                            while stream.read(&mut [0_u8; 65536]).await.is_ok_and(|count| count > 0) {
                                let refusal = match flag.load(Ordering::SeqCst) {
                                    15 => "403 Forbidden\r\nX-Graphite-Upload-Refusal: ownerMismatch",
                                    16 => "429 Too Many Requests\r\nX-Graphite-Upload-Refusal: clientFull",
                                    20 | 21 => "400 Bad Request\r\nX-Graphite-Upload-Refusal: invalid",
                                    _ => "",
                                };
                                if !refused && !refusal.is_empty() {
                                    refused = true;
                                    let _ = stream.write_all(format!("HTTP/1.1 {refusal}\r\nContent-Length: 0\r\n\r\n").as_bytes()).await;
                                }
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                            return;
                        }
                        let mode = flag.load(Ordering::SeqCst);
                        if request.starts_with(b"POST /upload/checkpoint") && matches!(mode, 0 | 5 | 7 | 15 | 16 | 17 | 19 | 20 | 21) {
                            if mode == 17 {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                            let bytes = match mode {
                                // The receiver takes nothing more.
                                7 | 16 => checkpoints.load(Ordering::SeqCst),
                                // The receiver counts 64 KiB more, and reports them, while its first checkpoint is in flight.
                                19 => {
                                    let bytes = *progress.borrow();
                                    if bytes == 1 {
                                        progress.send_replace(1 + (1 << 16));
                                        tokio::time::sleep(Duration::from_millis(100)).await;
                                    }
                                    bytes
                                }
                                _ => checkpoints.fetch_add(1 << 16, Ordering::SeqCst),
                            };
                            let body = format!(r#"{{"bytes":{bytes},"nanos":{}}}"#, receiver_clock.elapsed().as_nanos() + 1);
                            let _ = stream.write_all(crate::fixtures::ok(&body).as_bytes()).await;
                            return;
                        }
                        if !first_request.swap(true, Ordering::SeqCst)
                            && let Some(gate) = gate
                        {
                            starting.notify_one();
                            gate.wait().await;
                        }
                        if flag.load(Ordering::SeqCst) == 2 {
                            std::future::pending::<()>().await;
                            return;
                        }
                        if flag.load(Ordering::SeqCst) == 3 {
                            let _ = stream.write_all(format!("HTTP/1.1 403 Forbidden\r\nGraphite-Meter-Auth: required\r\nGraphite-Meter-Auth-Url: {login_url}\r\nContent-Length: 0\r\n\r\n").as_bytes()).await;
                            let _ = stream.shutdown().await;
                            return;
                        }
                        if flag.load(Ordering::SeqCst) == 14 {
                            let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n").await;
                            return;
                        }
                        if stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 68719476736\r\n\r\n").await.is_err() { return; }
                        if flag.load(Ordering::SeqCst) == 5 && request.windows(6).any(|value| value == b"lane=1") {
                            std::future::pending::<()>().await;
                        }
                        let bytes = [0_u8; 65536];
                        let mut started = false;
                        while !matches!(flag.load(Ordering::SeqCst), 3 | 14) && stream.write_all(&bytes).await.is_ok() {
                            if !started {
                                starting.notify_one();
                                started = true;
                            }
                            // Shared silence keeps its sockets open until the fixture drops them.
                            if flag.load(Ordering::SeqCst) == 23 {
                                std::future::pending::<()>().await;
                            }
                            // A bounded transfer cadence lets paused time advance between socket writes.
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                    });
                }
                _ = clients.join_next(), if !clients.is_empty() => {},
            }
        }
    });
    Ok((origin, failed, started))
}

async fn prepared_download(id: &str, origin: &str, http: &Http) -> Result<PreparedServer, Error> {
    Ok(PreparedServer {
        entry: ServerEntry {
            id: id.into(),
            url: origin.into(),
            name: id.into(),
            ..ServerEntry::default()
        },
        client: http.clone(),
        throughput: Some(ThroughputTarget {
            base_url: origin.into(),
            transport: ThroughputTransport::FetchStream,
            protocol: Protocol::Http1,
        }),
        http: Some(Arc::new(Transport::connect(http.clone(), origin, Protocol::Http1).await?)),
        latency: None,
        idle_rtt: Duration::ZERO,
        stage_limit: graphite_meter_core::discovery::DEFAULT_STAGE_LIMIT,
        replaced_upload: Arc::default(),
    })
}

/// Paused time leaps to the next timer whenever the runtime waits on a socket; a millisecond timer
/// keeps each leap to a millisecond, so loopback exchanges stay ahead of every stage deadline. Every
/// fixture runs one.
async fn heartbeat() {
    loop {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// The snapshot's one failure.
#[track_caller]
fn sole_failure(snapshot: &Snapshot) -> &crate::model::ServerFailure {
    let [failure] = &snapshot.failures[..] else {
        panic!("{:?}", snapshot.failures);
    };
    failure
}

/// `stage` beside `waiter`, which waits for one of its phases or failures: a stage that ends before
/// that fails the test at this bound instead of hanging it.
async fn joined<A, B>(stage: impl Future<Output = A>, waiter: impl Future<Output = B>) -> Result<(A, B), Error> {
    Ok(tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(stage, waiter) }).await?)
}

#[tokio::test]
async fn a_stopped_bidirectional_start_drains_its_started_download() -> Result<(), Error> {
    let (listener, origin) = crate::fixtures::listener().await?;
    let active = watch::channel(0_usize).0;
    let uploading = Arc::new(Notify::new());
    let active_server = active.clone();
    let upload_server = uploading.clone();
    let peer = tokio::spawn(async move {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((mut stream, _)) = accepted else { break; };
                    let active = active_server.clone();
                    let uploading = upload_server.clone();
                    clients.spawn(async move {
                        let mut request = [0_u8; 4096];
                        let Ok(length) = stream.read(&mut request).await else { return; };
                        if request[..length].starts_with(b"POST /upload/session") {
                            uploading.notify_one();
                            std::future::pending::<()>().await;
                        }
                        if !request[..length].starts_with(b"GET /download") { return; }
                        if stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 68719476736\r\n\r\n").await.is_err() { return; }
                        active.send_modify(|count| *count += 1);
                        let bytes = [0_u8; 65536];
                        while stream.write_all(&bytes).await.is_ok() {}
                        active.send_modify(|count| *count -= 1);
                    });
                }
                _ = clients.join_next(), if !clients.is_empty() => {},
            }
        }
    });
    let http = Http::new(true)?;
    let server = prepared_download("near", &origin, &http).await?;
    let config = Config {
        url: origin,
        stages: vec![Stage::Bidirectional],
        warmup: Duration::from_millis(10),
        streams: 1,
        ..Config::default()
    };
    let (stop, stopped) = watch::channel(false);
    let start = start_transfer(Stage::Bidirectional, &server, &config, Duration::from_secs(60), stopped);
    let stop_while_uploading = async {
        uploading.notified().await;
        stop.send_replace(true);
    };
    assert!(joined(start, stop_while_uploading).await?.0.is_err());
    let mut draining = active.subscribe();
    let drained = tokio::time::timeout(Duration::from_secs(1), draining.wait_for(|count| *count == 0));
    drained.await?.map(drop)?;
    peer.abort();
    Ok(())
}

#[tokio::test]
async fn first_stage_setup_failure_keeps_survivors_and_its_sign_in_cause() -> Result<(), Error> {
    let mut fixture = Fixture::new(&["near", "far"], 1400).await?;
    fixture.config.warmup = Duration::from_millis(10);
    let (servers, config, snapshots) = (&fixture.servers, &fixture.config, &fixture.snapshots);
    let observed = snapshots.subscribe();
    let stop = fixture.stop.subscribe();
    fixture.modes[0].store(3, Ordering::SeqCst);
    let mut ledger = RunLedger::new();
    let first = measure(Stage::Download, config, servers, snapshots, stop.clone(), &mut ledger).await?;
    assert_eq!(first, vec!["near"]);
    let failures = observed.borrow().failures.clone();
    assert_eq!(failures[0].reason, FailureReason::SignInRequired, "{failures:?}");
    let first_bytes = observed.borrow().results[0].down_bytes();
    assert!(first_bytes > 0);

    // A stalled first peer must not consume the next peer's startup budget.
    fixture.modes[0].store(2, Ordering::SeqCst);
    let second = measure(Stage::Download, config, servers, snapshots, stop, &mut ledger).await?;
    assert_eq!(second, vec!["near"]);
    let snapshot = observed.borrow();
    assert_eq!(snapshot.results.len(), 2);
    assert_eq!(snapshot.results[0].down_bytes(), first_bytes);
    assert!(snapshot.results[1].down_bytes() > 0);
    assert!(snapshot.results[1].down_bps().is_some());
    let contributions = &snapshot.results[1].server_results;
    assert_eq!(contributions.len(), 2);
    let summed: u64 = contributions.iter().map(|server| server.down_bytes()).sum();
    assert_eq!(summed, snapshot.results[1].down_bytes());
    assert!(contributions[0].down_bps().is_none());
    assert!(contributions[1].down_bps().is_some());
    Ok(())
}

/// Only probes the window sent count, with the replies the drain collects after it.
#[test]
fn host_latency_populations_and_continuity_are_independent() {
    let start = Instant::now();
    let end = start + Duration::from_secs(1);
    let window = Some((start, end));
    let mut hosts = BTreeMap::from(["near", "far"].map(|id| (id.to_owned(), HostLatency::default())));
    let sample = |sent, rtt_ms| Observation::Sample { sent, rtt: Duration::from_millis(rtt_ms), handling_nanos: 0 };
    for (id, rtt_ms) in [("near", 2), ("far", 200), ("near", 4), ("far", 220)] {
        observe(&mut hosts, window, (id.into(), sample(end - Duration::from_millis(10), rtt_ms)));
    }
    let near = hosts["near"].accumulator.snapshot();
    for sent in [start - Duration::from_millis(10), end] {
        observe(&mut hosts, window, ("near".into(), sample(sent, 20)));
    }
    assert_eq!(hosts["near"].accumulator.snapshot(), near);
    let far = hosts["far"].accumulator.snapshot();
    assert_eq!(near.count, 2);
    assert_eq!(far.count, 2);
    assert_eq!(near.jitter, Some(2_000_000));
    assert_eq!(far.jitter, Some(20_000_000));
    observe(&mut hosts, window, ("near".into(), Observation::ConnectionBoundary));
    // Go's live view counts the probes in a row that timed out.
    for _ in 0..2 {
        let sent = start + Duration::from_millis(20);
        let timeout = Observation::Lost { sent, outcome: ProbeOutcome::Timeout };
        observe(&mut hosts, window, ("far".into(), timeout));
    }
    let mut snapshot = Snapshot {
        server_latencies: ["near", "far"]
            .map(|id| ServerLatency { id: id.into(), ..ServerLatency::default() })
            .into(),
        ..Snapshot::default()
    };
    sample_hosts(&mut hosts, &mut snapshot);
    assert_eq!(snapshot.server_latencies[0].latest_ms, Some(4.0));
    assert_eq!(snapshot.server_latencies[1].latest_ms, Some(220.0));
    let streaks = snapshot.server_latencies.iter().map(|host| host.timeouts);
    assert_eq!(streaks.collect::<Vec<_>>(), [0, 2]);
    sample_hosts(&mut hosts, &mut snapshot);
    assert_eq!(snapshot.server_latencies[0].latest_ms, None);
    assert_eq!(snapshot.server_latencies[1].latest_ms, None);
}

#[tokio::test]
async fn loaded_latency_failure_keeps_every_http_participant() -> Result<(), Error> {
    for failure_phase in [Phase::Warmup, Phase::Measuring] {
        let mut fixture = Fixture::new(&["near", "far", "quiet"], 1200).await?;
        fixture.config.warmup = Duration::from_millis(500);
        fixture.config.loaded_latency = true;
        fixture.latency();
        fixture.modes[2].store(8, Ordering::SeqCst);
        // A channel ended as revoked is not dialled again (latency.go:248-249).
        let fail = async {
            fixture.phase(failure_phase).await;
            fixture.modes[0].store(9, Ordering::SeqCst);
        };
        let (result, ()) = joined(fixture.measure(Stage::Download), fail).await?;
        assert!(result?.is_empty());
        let snapshot = fixture.snapshots.borrow();
        let stage = &snapshot.results[0];
        assert_eq!(stage.server_results.len(), 3);
        assert!(stage.server_results.iter().all(|host| host.down_bytes() > 0));
        assert!(snapshot.failures.iter().any(|failure| failure.server_id == "near"));
        let mut participants = snapshot.intervals.iter().map(|interval| interval.participants.len());
        assert!(participants.all(|count| count == 3));
        let [near, far, quiet] =
            ["near", "far", "quiet"].map(|id| stage.server_latencies.iter().find(|host| host.id == id).unwrap());
        assert!(near.ending.is_some());
        assert!(far.ending.is_none());
        assert!(far.summary.count > 0);
        assert!(quiet.ending.is_none());
        assert!(quiet.summary.timeouts > 0);
    }
    Ok(())
}

#[tokio::test]
async fn mid_stage_auth_failure_keeps_reapproval_cause() -> Result<(), Error> {
    let fixture = Fixture::new(&["peer"], 3000).await?;

    let error = fixture.fault(Stage::Download, Duration::ZERO, 0, 3).await?.unwrap_err();
    assert!(crate::failure::sign_in(error.as_ref()).is_some(), "{error}");
    Ok(())
}

/// On worker threads, as the client runs: eight TLS lanes on one loaded thread kept both first
/// checkpoints past their 1.5 s budget, which ended the stage before its window.
#[tokio::test(flavor = "multi_thread")]
async fn silent_direction_removes_its_server_but_a_silent_lane_does_not() -> Result<(), Error> {
    let mut fixture = Fixture::new(&["near", "far"], 3000).await?;
    fixture.config.streams = 2;
    fixture.modes[0].store(5, Ordering::SeqCst);

    assert_eq!(fixture.fault(Stage::Bidirectional, Duration::ZERO, 1, 7).await??, ["far"]);
    let snapshot = fixture.snapshots.borrow();
    let result = &snapshot.results[0];
    assert_eq!(snapshot.failures.len(), 1);
    assert_eq!(snapshot.failures[0].reason, FailureReason::Timeout);
    assert!(result.server_results[0].down_bps().is_some());
    assert!(result.server_results[0].up_bps().is_some());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn shared_download_silence_keeps_a_sole_server_and_multiple_servers_until_the_stage_end() -> Result<(), Error> {
    for count in [1, 2] {
        let fixture = Fixture::new(&["peer-0", "peer-1"][..count], 4000).await?;
        let quiet = async {
            fixture.phase(Phase::Measuring).await;
            for mode in &fixture.modes {
                mode.store(23, Ordering::SeqCst);
            }
            tokio::time::sleep(REDIAL_WINDOW + STALL_QUIET).await;
            let snapshot = fixture.snapshots.borrow();
            assert!(
                snapshot.phase == Phase::Measuring && snapshot.failures.is_empty(),
                "shared silence ended the stage early: {snapshot:?}"
            );
        };
        let _ = joined(fixture.measure(Stage::Download), quiet).await?;
    }
    Ok(())
}

/// A lane still retrying at the final boundary removes its quiet server: a busy download, and
/// upload lanes answered busy as they send, which made no progress as Go's uploadLane counts it
/// (upload.go:118-128).
/// The 1.5 s window leaves the quiet server 500 ms before the 2 s stall rule would remove it first.
#[tokio::test]
async fn a_lane_still_retrying_at_the_final_boundary_removes_its_quiet_server() -> Result<(), Error> {
    for (stage, busy) in [(Stage::Download, 14), (Stage::Upload, 16)] {
        let fixture = Fixture::new(&["near", "far"], 1500).await?;

        let result = fixture.fault(stage, Duration::ZERO, 0, busy).await?;
        let removed = result.map_err(|error| format!("{stage:?}: {error}"))?;
        let snapshot = fixture.snapshots.borrow();
        let (failures, result) = (&snapshot.failures, &snapshot.results[0]);
        assert_eq!(removed, ["near"], "{stage:?}");
        let failure = (failures[0].server_id.as_str(), failures[0].reason);
        assert_eq!(failure, ("near", FailureReason::ServerBusy), "{stage:?}: {failures:?}");
        assert!(result.down_bps().or(result.up_bps()).is_some(), "{stage:?}");
        let intervals = &snapshot.intervals;
        let (first, last) = (&intervals[0], intervals.back().unwrap());
        let span = last.end_nanos - first.end_nanos;
        assert!(span >= 500_000_000, "{stage:?}: {intervals:?}");
        assert_eq!(last.participants, ["far"], "{stage:?}");
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_lane_refused_just_before_the_stage_end_takes_its_server_out() -> Result<(), Error> {
    let fixture = Fixture::new(&["near", "far"], 1500).await?;

    // No sample falls between the refusal and the stage end.
    let before_end = fixture.config.upload_duration - Duration::from_millis(100);
    assert_eq!(fixture.fault(Stage::Upload, before_end, 0, 15).await??, ["near"]);
    let snapshot = fixture.snapshots.borrow();
    let failure = sole_failure(&snapshot);
    assert_eq!(
        (failure.server_id.as_str(), failure.scope, failure.reason),
        ("near", FailureScope::Throughput, FailureReason::ProtocolError)
    );
    assert_eq!(snapshot.stage_status(&snapshot.results[0]), StageStatus::Partial);
    Ok(())
}

/// 3 s upload stages whose receiver forgets its upload id as the window opens, as a restarted server
/// does: in fixture mode 20 until the next mint, 21 for good. As Go's measureUpload (upload.go:23-31),
/// it is replaced once per server and run and the new id resumes the aggregate's evidence; forgotten
/// again, the server leaves on the refusal. Only the evidence after a replacement counts, as the
/// interval its new id ends is incomplete, so the replacement gets most of the window.
/// Runs on real time: a paused clock races ahead of the real sockets on a busy machine and closes the window
/// before its evidence arrives.
#[tokio::test]
async fn a_receiver_that_forgets_its_upload_is_replaced_once() -> Result<(), Error> {
    let once = Fixture::new(&["peer"], 3000).await?;
    let replaced = once.fault(Stage::Upload, Duration::ZERO, 0, 20).await?;
    let snapshot = once.snapshots.borrow();
    assert!(snapshot.failures.is_empty(), "{:?}", snapshot.failures);
    assert!(replaced?.is_empty());
    let resumed = graphite_meter_core::measurement::IntervalReason::EvidenceResumed;
    let intervals = &snapshot.intervals;
    assert!(intervals.iter().any(|interval| interval.reason == resumed), "{intervals:?}");
    assert!(snapshot.results[0].up_bps().is_some());
    drop(snapshot);
    let twice = Fixture::new(&["peer"], 3000).await?;
    assert!(twice.fault(Stage::Upload, Duration::ZERO, 0, 21).await?.is_err());
    let snapshot = twice.snapshots.borrow();
    let failure = sole_failure(&snapshot);
    assert_eq!((failure.scope, failure.reason), (FailureScope::Throughput, FailureReason::ProtocolError));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_receiver_without_a_first_checkpoint_fails_its_preparation() -> Result<(), Error> {
    let fixture = Fixture::new(&["near", "far"], 1000).await?;
    // The receiver accepts lanes but refuses every checkpoint.
    fixture.modes[1].store(14, Ordering::SeqCst);

    assert_eq!(fixture.measure(Stage::Upload).await?, ["far"]);
    let snapshot = fixture.snapshots.borrow();
    let failure = sole_failure(&snapshot);
    assert_eq!(
        (failure.server_id.as_str(), failure.scope, failure.reason),
        ("far", FailureScope::Throughput, FailureReason::PreparationFailed)
    );
    Ok(())
}

// This checks the receiver's byte baseline, not a virtual deadline. Its TLS feed needs real I/O.
#[tokio::test]
async fn an_upload_counts_what_its_receiver_took_during_the_first_checkpoint() -> Result<(), Error> {
    let fixture = Fixture::new(&["peer"], 1500).await?;
    fixture.modes[0].store(19, Ordering::SeqCst);

    assert!(fixture.measure(Stage::Upload).await?.is_empty());
    // Go's baseline is the upload observed before that checkpoint, not the progress reported after it.
    assert_eq!(fixture.snapshots.borrow().results[0].up_bytes(), 1 << 16);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_removal_at_the_final_boundary_collects_a_fresh_one_for_the_rest() -> Result<(), Error> {
    let fixture = Fixture::new(&["near", "far"], 2000).await?;
    fixture.modes[1].store(17, Ordering::SeqCst);

    // After the last sample's checkpoints, before the final boundary's.
    let before_end = fixture.config.upload_duration - Duration::from_millis(100);
    assert_eq!(fixture.fault(Stage::Upload, before_end, 0, 3).await??, ["near"]);
    let snapshot = fixture.snapshots.borrow();
    let failure = sole_failure(&snapshot);
    assert_eq!((failure.server_id.as_str(), failure.reason), ("near", FailureReason::SignInRequired));
    let intervals = &snapshot.intervals;
    let (first, last) = (&intervals[0], intervals.back().unwrap());
    assert_eq!(last.participants, ["far"]);
    // The fresh boundary starts once far's first final checkpoint has answered, 100 ms after the stage end.
    let fresh = fixture.config.upload_duration + Duration::from_millis(50);
    assert!(last.end_nanos - first.start_nanos >= fresh.as_nanos() as u64, "{intervals:?}");
    Ok(())
}

#[test]
fn checkpoints_skip_two_misses_reset_on_success_and_keep_final_misses() {
    let mut member = Member::new("peer".into(), None, false);
    let refused = || -> Option<Error> { Some("refused".into()) };
    for final_boundary in [false, false, true] {
        assert!(member.missed(refused(), final_boundary, true).is_none());
    }
    assert!(member.missed(None, false, true).is_none());
    assert!(member.missed(refused(), false, true).is_none());
    assert!(member.missed(refused(), false, true).is_none());
    assert!(member.missed(refused(), false, true).is_some());
    assert!(member.missed(refused(), false, false).is_none());
    let revoked = Failure::SignIn {
        origin: "https://meter.test".into(),
        login_url: "https://meter.test/login".into(),
    };
    member.checkpoint_misses = 0;
    assert!(member.missed(Some(Box::new(revoked)), true, false).is_some());
}

#[tokio::test(start_paused = true)]
async fn intervals_and_failures_share_the_run_clock() -> Result<(), Error> {
    let mut fixture = Fixture::new(&["near", "far"], 1000).await?;
    fixture.config.stages = vec![Stage::Download, Stage::Upload];
    fixture.config.warmup = Duration::from_millis(500);
    let (far_mode, mut observed) = (fixture.modes[1].clone(), fixture.snapshots.subscribe());
    // Far refuses the upload's checkpoints, so it fails as the upload window opens.
    let upload = |snapshot: &Snapshot| snapshot.stage == Some(Stage::Upload);
    let fault = async move {
        if observed.wait_for(upload).await.is_ok() {
            far_mode.store(14, Ordering::SeqCst);
        }
    };
    joined(fixture.run(), fault).await?.0?;
    let snapshot = fixture.snapshots.borrow();
    let failure = sole_failure(&snapshot);
    assert_eq!((failure.server_id.as_str(), failure.stage), ("far", Stage::Upload));
    // As Go's final() emits them, the final boundary's rates come after the upload's 1 s window.
    assert!(snapshot.latest.elapsed >= Duration::from_secs(1) && snapshot.latest.up_bps.is_some());
    // Go times both from the run's start: two warmups, the download window and the checkpoint budget.
    assert!(failure.at >= Duration::from_secs(3), "{:?}", failure.at);
    let report = crate::report::Report::new(&snapshot, None, crate::report::WIDTH, Default::default());
    let details: Vec<_> = report.details(true).iter().map(crate::report::plain).collect();
    let details = details.join("\n");
    let (_, intervals) = details.split_once("Aggregation intervals\n").ok_or(details.clone())?;
    let [download, upload] = intervals.lines().collect::<Vec<_>>()[..] else {
        panic!("{details}");
    };
    assert!(download.starts_with("Download ") && download.contains(" · near, far · "), "{details}");
    let upload = upload
        .strip_prefix("Upload ")
        .and_then(|line| line.split_once('–'))
        .and_then(|(start, _)| start.parse::<f64>().ok())
        .ok_or(details.clone())?;
    assert!(
        (upload - failure.at.as_secs_f64()).abs() <= 0.1,
        "the upload window opened at {upload} s, its failure at {:?}: {details}",
        failure.at
    );
    Ok(())
}

#[tokio::test]
async fn latency_stage_losses_drop_one_server_and_the_run_continues() -> Result<(), Error> {
    let mut fixture = Fixture::new(&["near", "far"], 1000).await?;
    let far_mode = fixture.modes[1].clone();
    // Far answers no probe, and loses its channel once near, never dialled, has left: as in Go,
    // near tries for 2 s first (latency.go:141).
    far_mode.store(8, Ordering::SeqCst);
    // Keep the unresponsive endpoint bound so parallel fixtures cannot reuse its port.
    let unresponsive = TcpListener::bind("127.0.0.1:0").await?;
    let unresponsive_url = format!("http://{}", unresponsive.local_addr()?);
    fixture.latency();
    fixture.servers[0].latency.as_mut().unwrap().base_url = unresponsive_url;
    fixture.config.stages = vec![Stage::Latency, Stage::Download];
    let mut observed = fixture.snapshots.subscribe();
    let far_loses_later = async {
        let near_lost = |snapshot: &Snapshot| snapshot.failures.iter().any(|failure| failure.server_id == "near");
        let lost = observed.wait_for(near_lost).await.map(drop);
        far_mode.store(6, Ordering::SeqCst);
        lost
    };
    let (result, lost) = joined(fixture.run(), far_loses_later).await?;
    result?;
    lost?;
    let snapshot = fixture.snapshots.borrow();
    assert_eq!(snapshot.phase, Phase::Incomplete);
    let [latency, download] = &snapshot.results[..] else {
        panic!("expected latency and download results");
    };
    assert_eq!(snapshot.stage_status(latency), StageStatus::Failed);
    assert_eq!(download.server_results.len(), 1);
    assert!(download.down_bytes() > 0);
    assert_eq!(
        snapshot.participants,
        ["far"],
        "a server lost before its latency channel dialled stayed in the run"
    );
    assert_eq!(download.server_results[0].id, "far");
    Ok(())
}

#[tokio::test]
async fn a_latency_stage_charts_the_replies_its_drain_collects() -> Result<(), Error> {
    let mut fixture = Fixture::new(&["near"], 1000).await?;
    fixture.latency();
    fixture.config.stages = vec![Stage::Latency];
    fixture.config.ping_interval = Duration::from_millis(100);
    fixture.run().await?;
    let snapshot = fixture.snapshots.borrow();
    // As Go charts replies through the drain, the stage's last sample comes after its 1 s window.
    assert!(snapshot.latest.elapsed >= Duration::from_secs(1), "{:?}", snapshot.latest);
    Ok(())
}

#[tokio::test]
async fn a_stop_during_readiness_sends_the_upload_delete() -> Result<(), Error> {
    let mut fixture = Fixture::new(&["peer"], 1000).await?;
    fixture.config.warmup = Duration::from_secs(2);
    fixture.modes[0].store(11, Ordering::SeqCst);

    let request_stop = async {
        while fixture.modes[0].load(Ordering::SeqCst) != 12 {
            tokio::task::yield_now().await;
        }
        fixture.stop.send_replace(true);
    };
    let stopped = async { tokio::join!(fixture.measure(Stage::Upload), request_stop) };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), stopped).await?;
    assert!(result?.is_empty());
    assert_eq!(fixture.modes[0].load(Ordering::SeqCst), 13);
    Ok(())
}

// Real time: on a paused clock that auto-advances while real sockets are read, the latency channel's 2 s dial
// window can pass during its TLS setup, and a probe's 250 ms deadline before its loopback pong arrives.
#[tokio::test]
async fn a_stop_records_the_evidence_its_stage_lacked() -> Result<(), Error> {
    for (stage, scope) in [(Stage::Download, FailureScope::Throughput), (Stage::Latency, FailureScope::Latency)] {
        let mut fixture = Fixture::new(&["peer"], 2000).await?;
        fixture.latency();
        fixture.config.ping_interval = Duration::from_millis(100);
        fixture.modes[0].store(8, Ordering::SeqCst);

        let stop_early = async {
            fixture.phase(Phase::Measuring).await;
            // Well inside the first probe's 250 ms deadline, so no probe can time out first.
            tokio::time::sleep(Duration::from_millis(50)).await;
            fixture.stop.send_replace(true);
        };
        let (result, ()) = joined(fixture.measure(stage), stop_early).await?;
        assert!(result?.is_empty());
        let snapshot = fixture.snapshots.borrow();
        assert!(snapshot.results[0].stopped);
        let failure = sole_failure(&snapshot);
        assert_eq!(
            (failure.server_id.as_str(), failure.stage, failure.scope, failure.reason),
            ("peer", stage, scope, FailureReason::InsufficientEvidence)
        );
    }
    Ok(())
}

/// A channel lost once the window has ended, before the stage drains, ends its session as the
/// window's end does, with no failure, as Go's probes.ended leads to finish(nil) (latency.go:252-253).
/// The stage tells each session its window as it opens (latency.go:233) and counts the session stopped.
#[tokio::test]
async fn a_loss_after_the_window_end_ends_the_session_with_no_failure() -> Result<(), Error> {
    let mut fixture = Fixture::new(&["peer"], 300).await?;
    fixture.latency();
    fixture.config.ping_interval = Duration::from_millis(20);
    let (mut ledger, config, servers) = (RunLedger::new(), &fixture.config, &fixture.servers);
    let mut run = StageRun::open(Stage::Latency, config, servers, &fixture.snapshots, &mut ledger)?;
    run.ready().await?;
    let end = run.open_window().await?;
    // The peer ends the channel as idle 30 ms after the window's end, and no drain follows.
    tokio::time::sleep_until(end + Duration::from_millis(30)).await;
    fixture.modes[0].store(6, Ordering::SeqCst);
    let ended = tokio::time::timeout(Duration::from_secs(5), run.latency.join_next()).await?;
    run.latency_ended(ended.ok_or("no latency session")?)?;
    drop(run);
    let snapshot = fixture.snapshots.borrow();
    assert!(snapshot.failures.is_empty(), "{:?}", snapshot.failures);
    Ok(())
}

/// A member's lane lost while another member still starts is noticed when it is lost, as Go's
/// ready handles each outcome as it arrives (stage.go:240-289), not once every start has ended.
#[tokio::test]
async fn a_lane_lost_while_another_member_starts_is_noticed_at_once() -> Result<(), Error> {
    let gate = Arc::new(Barrier::new(2));
    let fixture = Fixture::with_gates(&["near", "far"], 1000, &[None, Some(gate.clone())]).await?;
    let mut observed = fixture.snapshots.subscribe();
    // Near's started lane asks for sign-in while far's start waits at its gate, which opens once the
    // loss is recorded, or after 5 s.
    let revoke_near = async {
        tokio::join!(fixture.started[0].notified(), fixture.started[1].notified());
        fixture.modes[0].store(3, Ordering::SeqCst);
        let failed = |snapshot: &Snapshot| !snapshot.failures.is_empty();
        let noticed = tokio::time::timeout(Duration::from_secs(5), observed.wait_for(failed))
            .await
            .is_ok();
        gate.wait().await;
        noticed
    };
    let (result, noticed) = joined(fixture.measure(Stage::Download), revoke_near).await?;
    assert!(noticed, "{:?}", observed.borrow().failures);
    assert_eq!(result?, ["near"]);
    Ok(())
}

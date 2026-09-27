use super::*;
use crate::transport::Transport;
use crate::{
    model::{ServerSummary, StageStatus},
    net::Http,
};
use graphite_meter_core::discovery::Protocol;
use graphite_meter_core::{catalog::ServerEntry, discovery::ThroughputTarget};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Barrier,
    task::JoinHandle,
};

use crate::test_identity;

async fn download_peer() -> Result<(String, Arc<AtomicU8>, JoinHandle<()>), Error> {
    download_peer_with_gate(None).await
}

async fn download_peer_with_gate(gate: Option<Arc<Barrier>>) -> Result<(String, Arc<AtomicU8>, JoinHandle<()>), Error> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("https://{}", listener.local_addr()?);
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    let (certificate, key) = test_identity::generate_identity("localhost")?;
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(crate::crypto::provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from_pem_slice(certificate.as_bytes())?],
            PrivateKeyDer::from_pem_slice(key.as_bytes())?,
        )?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let failed = Arc::new(AtomicU8::new(0));
    let flag = failed.clone();
    let first_request = Arc::new(AtomicBool::new(false));
    let checkpoints = Arc::new(AtomicU64::new(0));
    let finalized = Arc::new(AtomicBool::new(false));
    let receiver_clock = std::time::Instant::now();
    let login_url = format!("{origin}/login");
    let server = tokio::spawn(async move {
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
                    let finalized = finalized.clone();
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
                                if flag.load(Ordering::SeqCst) == 6 {
                                    let ending = graphite_meter_core::failure::LaneEnding::Idle;
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
                        if request.starts_with(b"GET /servers ") {
                            let body = serde_json::json!({"defaultSelection":["self","gone"],"servers":[{"id":"self","url":".","name":"self"},{"id":"gone","url":"http://127.0.0.1:1","name":"gone"}]}).to_string();
                            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                            let _ = stream.write_all(response.as_bytes()).await;
                            return;
                        }
                        if request.starts_with(b"GET /preflight ") || request.starts_with(b"GET /probe ") {
                            let body = if request.starts_with(b"GET /preflight ") {
                                serde_json::json!({"generation":"fixture","capabilities":{"uploadCheckpoint":true,"throughput":[{"baseUrl":".","transport":"fetch-stream","protocol":"http1"}],"latency":[]}})
                            } else { serde_json::json!({"clientIp":"127.0.0.1","clientIpVersion":4,"clientIpSource":"socket","protocolNegotiated":"http/1.1"}) }.to_string();
                            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                            let _ = stream.write_all(response.as_bytes()).await;
                            return;
                        }
                        if flag.load(Ordering::SeqCst) == 1 && request.starts_with(b"POST /upload/session") {
                            let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                            return;
                        }
                        if request.starts_with(b"POST /upload/session") {
                            finalized.store(false, Ordering::SeqCst);
                            let body = br#"{"uploadId":"test-session"}"#;
                            let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                            let _ = stream.write_all(header.as_bytes()).await;
                            let _ = stream.write_all(body).await;
                            return;
                        }
                        if request.starts_with(b"GET /upload/progress") {
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 68719476736\r\n\r\n{\"type\":\"ready\"}\n").await;
                            if flag.compare_exchange(11, 12, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                                while !finalized.load(Ordering::SeqCst) {
                                    tokio::time::sleep(Duration::from_millis(5)).await;
                                }
                            }
                            while (matches!(flag.load(Ordering::SeqCst), 9 | 10) || !finalized.load(Ordering::SeqCst)) && stream.write_all(b"{\"type\":\"progress\",\"bytes\":1,\"nanos\":1}\n").await.is_ok() {
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                            let _ = stream.write_all(b"{\"type\":\"complete\",\"bytes\":1,\"nanos\":1}\n").await;
                            return;
                        }
                        if request.starts_with(b"DELETE /upload/progress") {
                            finalized.store(true, Ordering::SeqCst);
                            let _ = flag.compare_exchange(9, 10, Ordering::SeqCst, Ordering::SeqCst);
                            let _ = flag.compare_exchange(12, 13, Ordering::SeqCst, Ordering::SeqCst);
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
                            return;
                        }
                        if request.starts_with(b"POST /upload?") {
                            while stream.read(&mut [0_u8; 65536]).await.is_ok_and(|count| count > 0) {}
                            return;
                        }
                        let mode = flag.load(Ordering::SeqCst);
                        if request.starts_with(b"POST /upload/checkpoint") && matches!(mode, 0 | 5 | 7) {
                            let bytes = if mode == 7 { checkpoints.load(Ordering::SeqCst) } else { checkpoints.fetch_add(1 << 16, Ordering::SeqCst) };
                            let body = format!(r#"{{"bytes":{bytes},"nanos":{}}}"#, receiver_clock.elapsed().as_nanos() + 1);
                            let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                            let _ = stream.write_all(header.as_bytes()).await;
                            let _ = stream.write_all(body.as_bytes()).await;
                            return;
                        }
                        if !first_request.swap(true, Ordering::SeqCst)
                            && let Some(gate) = gate
                        {
                            gate.wait().await;
                        }
                        if flag.load(Ordering::SeqCst) == 2 {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                            return;
                        }
                        if flag.load(Ordering::SeqCst) == 3 {
                            let _ = stream.write_all(format!("HTTP/1.1 403 Forbidden\r\nGraphite-Meter-Auth: required\r\nGraphite-Meter-Auth-Url: {login_url}\r\nContent-Length: 0\r\n\r\n").as_bytes()).await;
                            let _ = stream.shutdown().await;
                            return;
                        }
                        if matches!(flag.load(Ordering::SeqCst), 1 | 14) {
                            let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n").await;
                            return;
                        }
                        if stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 68719476736\r\n\r\n").await.is_err() { return; }
                        if flag.load(Ordering::SeqCst) == 5 && request.windows(6).any(|value| value == b"lane=1") {
                            std::future::pending::<()>().await;
                        }
                        let bytes = [0_u8; 65536];
                        while !matches!(flag.load(Ordering::SeqCst), 3 | 14) && stream.write_all(&bytes).await.is_ok() {
                            if flag.load(Ordering::SeqCst) == 4 {
                                std::future::pending::<()>().await;
                            }
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                    });
                }
                _ = clients.join_next(), if !clients.is_empty() => {},
            }
        }
    });
    Ok((origin, failed, server))
}

#[tokio::test]
async fn selected_peers_start_stage_together_and_keep_catalogue_order() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let gate = Arc::new(Barrier::new(2));
    let (near, _, near_task) = download_peer_with_gate(Some(gate.clone())).await?;
    let (far, _, far_task) = download_peer_with_gate(Some(gate)).await?;
    let http = Http::new(true)?;
    let servers = vec![
        prepared_download("near", &near, &http).await?,
        prepared_download("far", &far, &http).await?,
    ];
    let config = Config {
        url: near,
        servers: vec!["near".into(), "far".into()],
        stages: vec![Stage::Download],
        warmup: Duration::from_millis(10),
        download_duration: Duration::from_millis(1400),
        streams: 1,
        loaded_latency: false,
        ..Config::default()
    };
    let (snapshots, observed) = watch::channel(Snapshot {
        servers: servers
            .iter()
            .map(|server| ServerSummary {
                id: server.entry.id.clone(),
                name: server.entry.name.clone(),
                origin: server.entry.url.clone(),
                ..ServerSummary::default()
            })
            .collect(),
        ..Snapshot::default()
    });
    let (_stop, cancelled) = watch::channel(false);
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        measure(Stage::Download, &config, &servers, &snapshots, cancelled),
    )
    .await??;
    assert!(result.is_empty());
    let snapshot = observed.borrow();
    let stage = &snapshot.results[0];
    assert!(snapshot.failures.is_empty(), "{:?}", snapshot.failures);
    assert_eq!(stage.server_results[0].id, "near");
    assert_eq!(stage.server_results[1].id, "far");
    assert!(stage.server_results.iter().all(|server| server.down_bytes() > 0));
    near_task.abort();
    far_task.abort();
    Ok(())
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
        http: Some(Arc::new(
            Transport::connect(http.clone(), origin, Protocol::Http1, false).await?,
        )),
        latency: None,
        idle_rtt: Duration::ZERO,
    })
}

#[tokio::test]
async fn a_stopped_bidirectional_start_drains_its_started_download() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let active = Arc::new(AtomicUsize::new(0));
    let uploading = Arc::new(tokio::sync::Notify::new());
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
                        active.fetch_add(1, Ordering::SeqCst);
                        let bytes = [0_u8; 65536];
                        while stream.write_all(&bytes).await.is_ok() {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        active.fetch_sub(1, Ordering::SeqCst);
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
        loaded_latency: false,
        ..Config::default()
    };
    let plan = lane_plan(&config, Stage::Bidirectional, std::slice::from_ref(&server))?;
    let (stop, stopped) = watch::channel(false);
    let start = start_transfer(
        Stage::Bidirectional,
        &server,
        &plan,
        &config,
        Duration::from_secs(60),
        stopped,
    );
    let stop_while_uploading = async {
        uploading.notified().await;
        stop.send_replace(true);
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(start, stop_while_uploading)
    })
    .await?;
    assert!(result.is_err());
    tokio::time::timeout(Duration::from_secs(1), async {
        while active.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    peer.abort();
    Ok(())
}

#[tokio::test]
async fn first_stage_setup_failure_keeps_survivors_and_its_sign_in_cause() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (near, near_failed, near_task) = download_peer().await?;
    let (far, far_failed, far_task) = download_peer().await?;
    let http = Http::new(true)?;
    let servers = vec![
        prepared_download("near", &near, &http).await?,
        prepared_download("far", &far, &http).await?,
    ];
    let config = Config {
        url: near,
        servers: vec!["near".into(), "far".into()],
        stages: vec![Stage::Download],
        warmup: Duration::from_millis(10),
        download_duration: Duration::from_millis(1400),
        streams: 1,
        loaded_latency: false,
        ..Config::default()
    };
    let (snapshots, observed) = watch::channel(Snapshot {
        servers: servers
            .iter()
            .map(|server| ServerSummary {
                id: server.entry.id.clone(),
                name: server.entry.name.clone(),
                origin: server.entry.url.clone(),
                ..ServerSummary::default()
            })
            .collect(),
        ..Snapshot::default()
    });
    let (_stop, cancelled) = watch::channel(false);
    near_failed.store(3, Ordering::SeqCst);
    let first = measure(Stage::Download, &config, &servers, &snapshots, cancelled.clone()).await?;
    assert_eq!(first, vec!["near"]);
    assert_eq!(
        observed.borrow().failures[0].reason,
        graphite_meter_core::failure::FailureReason::SignInRequired,
        "{:?}",
        observed.borrow().failures
    );
    let first_bytes = observed.borrow().results[0].down_bytes();
    assert!(first_bytes > 0);

    // A stalled first peer must not consume the next peer's startup budget.
    near_failed.store(2, Ordering::SeqCst);
    let second = measure(Stage::Download, &config, &servers, &snapshots, cancelled.clone()).await?;
    assert_eq!(second, vec!["near"]);
    let snapshot = observed.borrow();
    assert_eq!(snapshot.results.len(), 2);
    assert_eq!(snapshot.results[0].down_bytes(), first_bytes);
    assert!(snapshot.results[1].down_bytes() > 0);
    assert!(snapshot.results[1].down_bps().is_some());
    let contributions = &snapshot.results[1].server_results;
    assert_eq!(contributions.len(), 2);
    assert_eq!(
        contributions.iter().map(|server| server.down_bytes()).sum::<u64>(),
        snapshot.results[1].down_bytes()
    );
    assert!(contributions[0].down_bps().is_none());
    assert!(contributions[1].down_bps().is_some());
    drop(snapshot);

    far_failed.store(1, Ordering::SeqCst);
    let third = measure(Stage::Download, &config, &servers[1..], &snapshots, cancelled).await;
    assert!(third.is_err());
    let snapshot = observed.borrow();
    assert_eq!(snapshot.results.len(), 3);
    assert_eq!(snapshot.results[0].down_bytes(), first_bytes);
    assert!(snapshot.results[2].down_bps().is_none());
    assert!(snapshot.failures.iter().any(|failure| failure.server_id == "far"));
    near_task.abort();
    far_task.abort();
    Ok(())
}

#[test]
fn replies_sent_before_stage_end_count_during_drain() {
    let start = Instant::now();
    let end = start + Duration::from_secs(1);
    let mut accumulator = LatencyAccumulator::default();
    let mut latest = None;
    observe_latency(
        Observation::Sample {
            sent: end - Duration::from_millis(10),
            received: end + Duration::from_millis(10),
            rtt: Duration::from_millis(20),
            server_handling: Duration::ZERO,
        },
        start,
        end,
        &mut accumulator,
        &mut latest,
    );
    let summary = accumulator.snapshot();
    assert_eq!(summary.count, 1);
    assert_eq!(summary.unresolved, 0);
    assert_eq!(latest, Some(20.0));
}

#[test]
fn warmup_and_poststage_probes_do_not_enter_measurement() {
    let start = Instant::now();
    let end = start + Duration::from_secs(1);
    let mut accumulator = LatencyAccumulator::default();
    let mut latest = None;
    for sent in [start - Duration::from_millis(10), end] {
        observe_latency(
            Observation::Sample {
                sent,
                received: sent + Duration::from_millis(1),
                rtt: Duration::from_millis(1),
                server_handling: Duration::ZERO,
            },
            start,
            end,
            &mut accumulator,
            &mut latest,
        );
    }
    assert_eq!(accumulator.snapshot(), Default::default());
}

#[test]
fn host_latency_populations_and_continuity_are_independent() {
    let start = Instant::now();
    let end = start + Duration::from_secs(1);
    let mut hosts = BTreeMap::from([
        ("near".to_owned(), HostLatency::default()),
        ("far".to_owned(), HostLatency::default()),
    ]);
    for (id, rtt_ms) in [("near", 2), ("far", 200), ("near", 4), ("far", 220)] {
        observe(
            &mut hosts,
            Some((start, end)),
            (
                id.into(),
                Observation::Sample {
                    sent: start + Duration::from_millis(10),
                    received: start + Duration::from_millis(10 + rtt_ms),
                    rtt: Duration::from_millis(rtt_ms),
                    server_handling: Duration::ZERO,
                },
            ),
        );
    }
    let near = hosts["near"].accumulator.snapshot();
    let far = hosts["far"].accumulator.snapshot();
    assert_eq!(near.count, 2);
    assert_eq!(far.count, 2);
    assert_eq!(near.jitter, Some(2_000_000));
    assert_eq!(far.jitter, Some(20_000_000));
    observe(
        &mut hosts,
        Some((start, end)),
        ("near".into(), Observation::ConnectionBoundary),
    );
    let mut snapshot = Snapshot {
        server_latencies: vec![
            ServerLatency {
                id: "near".into(),
                ..ServerLatency::default()
            },
            ServerLatency {
                id: "far".into(),
                ..ServerLatency::default()
            },
        ],
        ..Snapshot::default()
    };
    sample_hosts(&mut hosts, &mut snapshot, Duration::from_millis(500));
    assert_eq!(snapshot.server_latencies[0].latest_ms, Some(4.0));
    assert_eq!(snapshot.server_latencies[1].latest_ms, Some(220.0));
    sample_hosts(&mut hosts, &mut snapshot, Duration::from_secs(1));
    assert_eq!(snapshot.server_latencies[0].latest_ms, None);
    assert_eq!(snapshot.server_latencies[1].latest_ms, None);
}

#[tokio::test]
async fn loaded_latency_failure_keeps_every_http_participant() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    for failure_phase in [Phase::Warmup, Phase::Measuring] {
        let (near, mode, near_peer) = download_peer().await?;
        let (far, _, far_peer) = download_peer().await?;
        let (quiet, quiet_mode, quiet_peer) = download_peer().await?;
        quiet_mode.store(8, Ordering::SeqCst);
        let http = Http::new(true)?;
        let mut servers = vec![
            prepared_download("near", &near, &http).await?,
            prepared_download("far", &far, &http).await?,
            prepared_download("quiet", &quiet, &http).await?,
        ];
        for server in &mut servers {
            server.latency = Some(graphite_meter_core::discovery::LatencyTarget {
                base_url: server.entry.url.clone(),
                transport: LatencyTransport::WebSocket,
            });
        }
        let config = Config {
            insecure: true,
            warmup: Duration::from_millis(500),
            download_duration: Duration::from_millis(1200),
            streams: 1,
            ..Config::default()
        };
        let (snapshots, mut observed) = watch::channel(Snapshot::default());
        let (_stop, cancelled) = watch::channel(false);
        let run = measure(Stage::Download, &config, &servers, &snapshots, cancelled);
        let fail = async {
            while observed.borrow().phase != failure_phase {
                observed.changed().await.unwrap();
            }
            mode.store(6, Ordering::SeqCst);
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(run, fail) }).await?;
        assert!(result?.is_empty());
        let snapshot = observed.borrow();
        let stage = &snapshot.results[0];
        assert_eq!(stage.server_results.len(), 3);
        assert!(stage.server_results.iter().all(|host| host.down_bytes() > 0));
        assert!(snapshot.failures.iter().any(|failure| failure.server_id == "near"));
        assert!(stage.intervals.iter().all(|interval| interval.participants.len() == 3));
        let [near, far, quiet] =
            ["near", "far", "quiet"].map(|id| stage.server_latencies.iter().find(|host| host.id == id).unwrap());
        assert!(near.ending.is_some());
        assert!(far.ending.is_none());
        assert!(far.summary.count > 0);
        assert!(quiet.ending.is_none());
        assert!(quiet.summary.timeouts > 0);
        near_peer.abort();
        far_peer.abort();
        quiet_peer.abort();
    }
    Ok(())
}

#[tokio::test]
async fn mid_stage_auth_failure_keeps_reapproval_cause() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (origin, mode, peer) = download_peer().await?;
    let http = Http::new(true)?;
    let servers = vec![prepared_download("peer", &origin, &http).await?];
    let config = Config {
        warmup: Duration::ZERO,
        download_duration: Duration::from_secs(3),
        streams: 1,
        loaded_latency: false,
        ..Config::default()
    };
    let (snapshots, mut observed) = watch::channel(Snapshot::default());
    let (_stop, cancelled) = watch::channel(false);
    let revoke = async {
        observed
            .wait_for(|snapshot| snapshot.phase == Phase::Measuring)
            .await
            .unwrap();
        mode.store(3, Ordering::SeqCst);
    };
    let (result, ()) = tokio::join!(
        measure(Stage::Download, &config, &servers, &snapshots, cancelled,),
        revoke
    );
    peer.abort();
    let error = result.unwrap_err();
    assert!(crate::net::authentication_required(error.as_ref()).is_some(), "{error}");
    Ok(())
}

#[tokio::test]
async fn silent_direction_removes_its_server_but_a_silent_lane_does_not() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (near, near_mode, near_task) = download_peer().await?;
    let (far, far_mode, far_task) = download_peer().await?;
    near_mode.store(5, Ordering::SeqCst);
    let http = Http::new(true)?;
    let servers = vec![
        prepared_download("near", &near, &http).await?,
        prepared_download("far", &far, &http).await?,
    ];
    let config = Config {
        warmup: Duration::ZERO,
        bidirectional_duration: Duration::from_secs(3),
        streams: 2,
        loaded_latency: false,
        ..Config::default()
    };
    let (snapshots, mut observed) = watch::channel(Snapshot {
        servers: servers
            .iter()
            .map(|server| ServerSummary {
                id: server.entry.id.clone(),
                ..ServerSummary::default()
            })
            .collect(),
        ..Snapshot::default()
    });
    let (_stop, cancelled) = watch::channel(false);
    let stall_upload = async {
        observed
            .wait_for(|snapshot| snapshot.phase == Phase::Measuring)
            .await
            .unwrap();
        far_mode.store(7, Ordering::SeqCst);
    };
    let (result, ()) = tokio::join!(
        measure(Stage::Bidirectional, &config, &servers, &snapshots, cancelled),
        stall_upload
    );
    near_task.abort();
    far_task.abort();
    assert_eq!(result?, ["far"]);
    let snapshot = observed.borrow();
    let result = &snapshot.results[0];
    assert_eq!(snapshot.failures.len(), 1);
    assert_eq!(
        snapshot.failures[0].reason,
        graphite_meter_core::failure::FailureReason::Timeout
    );
    assert!(result.server_results[0].down_bps().is_some());
    assert!(result.server_results[0].up_bps().is_some());
    Ok(())
}

#[tokio::test]
async fn a_lane_still_retrying_at_the_final_boundary_removes_its_quiet_server() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (near, near_mode, near_task) = download_peer().await?;
    let (far, _, far_task) = download_peer().await?;
    let http = Http::new(true)?;
    let servers = vec![
        prepared_download("near", &near, &http).await?,
        prepared_download("far", &far, &http).await?,
    ];
    let config = Config {
        warmup: Duration::ZERO,
        download_duration: Duration::from_millis(1900),
        streams: 1,
        loaded_latency: false,
        ..Config::default()
    };
    let (snapshots, mut observed) = watch::channel(Snapshot::default());
    let (_stop, cancelled) = watch::channel(false);
    let refuse_near = async {
        observed
            .wait_for(|snapshot| snapshot.phase == Phase::Measuring)
            .await
            .unwrap();
        near_mode.store(14, Ordering::SeqCst);
    };
    let (result, ()) = tokio::join!(
        measure(Stage::Download, &config, &servers, &snapshots, cancelled),
        refuse_near
    );
    near_task.abort();
    far_task.abort();
    assert_eq!(result?, ["near"]);
    let snapshot = observed.borrow();
    let failure = &snapshot.failures[0];
    assert_eq!(
        (failure.server_id.as_str(), failure.reason),
        ("near", graphite_meter_core::failure::FailureReason::ServerBusy)
    );
    let stage = &snapshot.results[0];
    assert!(stage.down_bps().is_some());
    let (first, last) = (&stage.intervals[0], stage.intervals.back().unwrap());
    assert!(last.end_nanos - first.end_nanos >= 500_000_000, "{:?}", stage.intervals);
    assert_eq!(last.participants, ["far"]);
    Ok(())
}

#[test]
fn checkpoints_skip_two_misses_reset_on_success_and_keep_final_misses() {
    let mut member = Member {
        id: "peer".into(),
        stop: watch::channel(false).0,
        latency: None,
        starting: false,
        dialled: false,
        latency_failed: false,
        lanes: Lanes::default(),
        checkpoint_misses: 0,
        moved: [Instant::now(); 2],
    };
    let refused = || -> Option<Error> { Some("refused".into()) };
    for final_boundary in [false, false, true] {
        assert!(member.missed(refused(), final_boundary).is_none());
    }
    assert!(member.missed(None, false).is_none());
    assert!(member.missed(refused(), false).is_none());
    assert!(member.missed(refused(), false).is_none());
    assert!(member.missed(refused(), false).is_some());
    let revoked = crate::net::AuthRequired {
        origin: "https://meter.test".into(),
        login_url: "https://meter.test/login".into(),
    };
    member.checkpoint_misses = 0;
    assert!(member.missed(Some(Box::new(revoked)), true).is_some());
}

#[tokio::test]
async fn sole_server_reprepares_after_a_failed_stage_and_keeps_prior_evidence() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (origin, fault, peer) = download_peer().await?;
    let http = Http::new(true)?;
    let server = prepared_download("self", &origin, &http).await?;
    let config = Config {
        url: origin,
        stages: vec![Stage::Download, Stage::Upload, Stage::Download],
        warmup: Duration::ZERO,
        download_duration: Duration::from_secs(1),
        upload_duration: Duration::from_secs(1),
        loaded_latency: false,
        streams: 1,
        insecure: true,
        ..Config::default()
    };
    let prepared = super::super::PreparedRun {
        servers: vec![server],
        key: config.preparation_key(),
        verified_at: Instant::now(),
    };
    let (snapshots, mut observed) = watch::channel(Snapshot {
        servers: vec![ServerSummary {
            id: "self".into(),
            name: "fixture".into(),
            ..ServerSummary::default()
        }],
        ..Snapshot::default()
    });
    let drive_fault = tokio::spawn(async move {
        loop {
            if observed.borrow().stage == Some(Stage::Upload) {
                fault.store(1, Ordering::SeqCst);
            }
            if observed.borrow().results.len() >= 2 {
                fault.store(0, Ordering::SeqCst);
                return;
            }
            if observed.changed().await.is_err() {
                return;
            }
        }
    });
    let (_stop, cancelled) = watch::channel(false);
    super::super::run_prepared(config, http, snapshots.clone(), cancelled, Some(prepared)).await?;
    drive_fault.await?;
    let snapshot = snapshots.borrow();
    assert_eq!(snapshot.phase, Phase::Incomplete);
    assert_eq!(snapshot.results.len(), 3);
    assert_eq!(snapshot.stage_status(&snapshot.results[0]), StageStatus::Complete);
    assert!(snapshot.results[0].down_bytes() > 0);
    assert_eq!(snapshot.stage_status(&snapshot.results[1]), StageStatus::Failed);
    assert_eq!(snapshot.stage_status(&snapshot.results[2]), StageStatus::Complete);
    assert!(snapshot.results[2].down_bytes() > 0);
    assert!(snapshot.failures[0].at >= snapshot.results[0].elapsed);
    assert_eq!(
        snapshot.failures[0].reason,
        graphite_meter_core::failure::FailureReason::ServerBusy
    );
    peer.abort();
    Ok(())
}

#[tokio::test]
async fn a_selection_that_lost_a_server_in_preparation_gets_no_sole_retry() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (origin, mode, peer) = download_peer().await?;
    let config = Config {
        url: origin,
        stages: vec![Stage::Download, Stage::Upload],
        warmup: Duration::ZERO,
        download_duration: Duration::from_secs(3),
        upload_duration: Duration::from_secs(1),
        loaded_latency: false,
        streams: 1,
        insecure: true,
        ..Config::default()
    };
    let (snapshots, mut observed) = watch::channel(Snapshot::default());
    let (_stop, cancelled) = watch::channel(false);
    let refuse = async {
        observed
            .wait_for(|snapshot| snapshot.phase == Phase::Measuring)
            .await
            .unwrap();
        mode.store(14, Ordering::SeqCst);
    };
    let run = super::super::run_prepared(config, Http::new(true)?, snapshots.clone(), cancelled, None);
    let (result, ()) = tokio::join!(run, refuse);
    peer.abort();
    assert!(
        result.is_err(),
        "the prepared server of two was retried as a sole server"
    );
    let snapshot = snapshots.borrow();
    assert_eq!(snapshot.results.len(), 1);
    assert_eq!(snapshot.failures[0].server_id, "gone");
    Ok(())
}

#[tokio::test]
async fn the_latency_result_follows_the_focus_server() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    for (silent_focus, outcome) in [(false, Phase::Partial), (true, Phase::Incomplete)] {
        let (near, near_mode, near_peer) = download_peer().await?;
        let (far, far_mode, far_peer) = download_peer().await?;
        let silent = if silent_focus { "near" } else { "far" };
        [near_mode, far_mode][usize::from(!silent_focus)].store(8, Ordering::SeqCst);
        let http = Http::new(true)?;
        let mut servers = vec![
            prepared_download("near", &near, &http).await?,
            prepared_download("far", &far, &http).await?,
        ];
        for server in &mut servers {
            server.latency = Some(graphite_meter_core::discovery::LatencyTarget {
                base_url: server.entry.url.clone(),
                transport: LatencyTransport::WebSocket,
            });
        }
        let config = Config {
            stages: vec![Stage::Latency],
            warmup: Duration::ZERO,
            latency_duration: Duration::from_secs(1),
            ping_interval: Duration::from_millis(100),
            insecure: true,
            ..Config::default()
        };
        let prepared = super::super::PreparedRun {
            servers,
            key: config.preparation_key(),
            verified_at: Instant::now(),
        };
        let (snapshots, _) = watch::channel(Snapshot::default());
        let (_stop, cancelled) = watch::channel(false);
        super::super::run_prepared(config, http, snapshots.clone(), cancelled, Some(prepared)).await?;
        near_peer.abort();
        far_peer.abort();
        let snapshot = snapshots.borrow();
        assert_eq!(snapshot.latency_focus.as_deref(), Some("near"));
        assert_eq!(
            snapshot.phase, outcome,
            "silent focus {silent_focus}: {:?}",
            snapshot.failures
        );
        let [failure] = &snapshot.failures[..] else {
            panic!("{:?}", snapshot.failures);
        };
        assert_eq!(
            (failure.server_id.as_str(), failure.scope, failure.reason),
            (
                silent,
                crate::model::FailureScope::Latency,
                graphite_meter_core::failure::FailureReason::InsufficientEvidence
            )
        );
    }
    Ok(())
}

#[tokio::test]
async fn latency_stage_losses_drop_one_server_and_the_run_continues() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (near, _, near_peer) = download_peer().await?;
    let (far, far_mode, far_peer) = download_peer().await?;
    far_mode.store(6, Ordering::SeqCst);
    let refused = format!("http://{}", TcpListener::bind("127.0.0.1:0").await?.local_addr()?);
    let http = Http::new(true)?;
    let mut servers = vec![
        prepared_download("near", &near, &http).await?,
        prepared_download("far", &far, &http).await?,
    ];
    for (server, latency) in servers.iter_mut().zip([&refused, &far]) {
        server.latency = Some(graphite_meter_core::discovery::LatencyTarget {
            base_url: latency.clone(),
            transport: LatencyTransport::WebSocket,
        });
    }
    let config = Config {
        url: near,
        servers: vec!["near".into(), "far".into()],
        stages: vec![Stage::Latency, Stage::Download],
        warmup: Duration::ZERO,
        latency_duration: Duration::from_secs(1),
        download_duration: Duration::from_secs(1),
        streams: 1,
        loaded_latency: false,
        insecure: true,
        ..Config::default()
    };
    let prepared = super::super::PreparedRun {
        servers,
        key: config.preparation_key(),
        verified_at: Instant::now(),
    };
    let (snapshots, _) = watch::channel(Snapshot {
        servers: ["near", "far"]
            .map(|id| ServerSummary {
                id: id.into(),
                ..ServerSummary::default()
            })
            .into(),
        ..Snapshot::default()
    });
    let (_stop, cancelled) = watch::channel(false);
    super::super::run_prepared(config, http, snapshots.clone(), cancelled, Some(prepared)).await?;
    near_peer.abort();
    far_peer.abort();
    let snapshot = snapshots.borrow();
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
    assert_eq!(
        snapshot.latency_focus.as_deref(),
        Some("near"),
        "the focus moves only to a survivor that measured latency"
    );
    Ok(())
}

#[tokio::test]
async fn a_stop_during_readiness_sends_the_upload_delete() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (origin, mode, peer) = download_peer().await?;
    mode.store(11, Ordering::SeqCst);
    let http = Http::new(true)?;
    let servers = vec![prepared_download("peer", &origin, &http).await?];
    let config = Config {
        warmup: Duration::from_secs(2),
        upload_duration: Duration::from_secs(1),
        streams: 1,
        loaded_latency: false,
        ..Config::default()
    };
    let (snapshots, _) = watch::channel(Snapshot::default());
    let (stop, cancelled) = watch::channel(false);
    let request_stop = async {
        while mode.load(Ordering::SeqCst) != 12 {
            tokio::task::yield_now().await;
        }
        stop.send_replace(true);
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(
            measure(Stage::Upload, &config, &servers, &snapshots, cancelled),
            request_stop
        )
    })
    .await?;
    peer.abort();
    assert!(result?.is_empty());
    assert_eq!(mode.load(Ordering::SeqCst), 13);
    Ok(())
}

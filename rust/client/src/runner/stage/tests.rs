use super::*;
use crate::transport::Transport;
use crate::{model::ServerSummary, net::Http};
use graphite_meter_core::discovery::Protocol;
use graphite_meter_core::{catalog::ServerEntry, discovery::ThroughputTarget};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Barrier,
    task::JoinHandle,
};

async fn download_peer() -> Result<(String, Arc<AtomicU8>, JoinHandle<()>), Error> {
    download_peer_with_gate(None).await
}

async fn download_peer_with_gate(
    gate: Option<Arc<Barrier>>,
) -> Result<(String, Arc<AtomicU8>, JoinHandle<()>), Error> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let failed = Arc::new(AtomicU8::new(0));
    let flag = failed.clone();
    let first_request = Arc::new(AtomicBool::new(false));
    let server = tokio::spawn(async move {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((mut stream, _)) = accepted else { break; };
                    let flag = flag.clone();
                    let gate = gate.clone();
                    let first_request = first_request.clone();
                    clients.spawn(async move {
                        let mut request = [0_u8; 4096];
                        if stream.read(&mut request).await.is_err() { return; }
                        if !first_request.swap(true, Ordering::SeqCst)
                            && let Some(gate) = gate
                        {
                            gate.wait().await;
                        }
                        if flag.load(Ordering::SeqCst) == 2 {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                            return;
                        }
                        if flag.load(Ordering::SeqCst) == 1 {
                            let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n").await;
                            return;
                        }
                        if stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 68719476736\r\n\r\n").await.is_err() { return; }
                        let bytes = [0_u8; 65536];
                        while stream.write_all(&bytes).await.is_ok() {
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
    let http = Http::new(false)?;
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
        measure(
            Stage::Download,
            &config,
            &servers,
            &snapshots,
            cancelled,
            false,
        ),
    )
    .await??;
    assert!(result.is_empty());
    let snapshot = observed.borrow();
    let stage = &snapshot.results[0];
    assert!(stage.complete);
    assert_eq!(stage.server_results[0].id, "near");
    assert_eq!(stage.server_results[1].id, "far");
    assert!(
        stage
            .server_results
            .iter()
            .all(|server| server.down_bytes > 0)
    );
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
async fn timed_out_bidirectional_setup_drains_started_download() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let active = Arc::new(AtomicUsize::new(0));
    let saw_upload = Arc::new(AtomicBool::new(false));
    let active_server = active.clone();
    let upload_server = saw_upload.clone();
    let peer = tokio::spawn(async move {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((mut stream, _)) = accepted else { break; };
                    let active = active_server.clone();
                    let saw_upload = upload_server.clone();
                    clients.spawn(async move {
                        let mut request = [0_u8; 4096];
                        let Ok(length) = stream.read(&mut request).await else { return; };
                        if request[..length].starts_with(b"POST /upload/session") {
                            saw_upload.store(true, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_secs(30)).await;
                            return;
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
    let http = Http::new(false)?;
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
    let (_stop, stopped) = watch::channel(false);
    let result = start_transfer(
        Stage::Bidirectional,
        &server,
        &plan,
        &config,
        StageTiming {
            epoch: Instant::now(),
            operation_limit: Duration::from_secs(60),
            setup_timeout: Duration::from_millis(300),
        },
        stopped,
    )
    .await;
    assert!(result.is_err());
    assert!(saw_upload.load(Ordering::SeqCst));
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
async fn later_preparation_dropout_preserves_prior_results_and_survivor_bytes() -> Result<(), Error>
{
    let _ = crate::crypto::provider().install_default();
    let (near, near_failed, near_task) = download_peer().await?;
    let (far, far_failed, far_task) = download_peer().await?;
    let http = Http::new(false)?;
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
    let first = measure(
        Stage::Download,
        &config,
        &servers,
        &snapshots,
        cancelled.clone(),
        false,
    )
    .await?;
    assert!(first.is_empty());
    assert!(observed.borrow().results[0].complete);
    let first_bytes = observed.borrow().results[0].down_bytes;
    assert!(first_bytes > 0);
    {
        let snapshot = observed.borrow();
        let first = &snapshot.results[0];
        assert_eq!(first.server_results.len(), 2);
        assert_eq!(
            first
                .server_results
                .iter()
                .map(|server| server.down_bytes)
                .sum::<u64>(),
            first.down_bytes
        );
        assert!(
            first
                .server_results
                .iter()
                .all(|server| server.down_bps.is_some())
        );
    }

    // A stalled first peer must not consume the next peer's startup budget.
    near_failed.store(2, Ordering::SeqCst);
    let second = measure(
        Stage::Download,
        &config,
        &servers,
        &snapshots,
        cancelled.clone(),
        true,
    )
    .await?;
    assert_eq!(second, vec!["near"]);
    let snapshot = observed.borrow();
    assert_eq!(snapshot.results.len(), 2);
    assert!(snapshot.results[0].complete);
    assert_eq!(snapshot.results[0].down_bytes, first_bytes);
    assert!(!snapshot.results[1].complete);
    assert!(snapshot.results[1].down_bytes > 0);
    assert!(snapshot.results[1].down_bps.is_some());
    let contributions = &snapshot.results[1].server_results;
    assert_eq!(contributions.len(), 2);
    assert_eq!(
        contributions
            .iter()
            .map(|server| server.down_bytes)
            .sum::<u64>(),
        snapshot.results[1].down_bytes
    );
    assert!(contributions[0].down_bps.is_none());
    assert!(contributions[0].error.is_some());
    assert!(contributions[1].down_bps.is_some());
    assert!(snapshot.servers[0].error.is_some());
    assert!(snapshot.servers[1].error.is_none());
    drop(snapshot);

    far_failed.store(1, Ordering::SeqCst);
    let third = measure(
        Stage::Download,
        &config,
        &servers[1..],
        &snapshots,
        cancelled,
        true,
    )
    .await;
    assert!(third.is_err());
    let snapshot = observed.borrow();
    assert_eq!(snapshot.results.len(), 3);
    assert!(snapshot.results[0].complete);
    assert_eq!(snapshot.results[0].down_bytes, first_bytes);
    assert!(!snapshot.results[2].complete);
    assert!(snapshot.servers[1].error.is_some());
    near_task.abort();
    far_task.abort();
    Ok(())
}

#[test]
fn replies_after_stage_end_remain_unresolved() {
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
    assert_eq!(summary.count, 0);
    assert_eq!(summary.unresolved, 1);
    assert_eq!(latest, None);
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
    assert!(!accumulator.snapshot().has_observations());
}

#[test]
fn host_latency_populations_and_continuity_are_independent() {
    let start = Instant::now();
    let end = start + Duration::from_secs(1);
    let mut measurements = LatencyMeasurements::default();
    measurements
        .hosts
        .insert("near".into(), HostLatency::default());
    measurements
        .hosts
        .insert("far".into(), HostLatency::default());
    for (id, rtt_ms) in [("near", 2), ("far", 200), ("near", 4), ("far", 220)] {
        measurements.observe(
            (
                id.into(),
                Observation::Sample {
                    sent: start + Duration::from_millis(10),
                    received: start + Duration::from_millis(10 + rtt_ms),
                    rtt: Duration::from_millis(rtt_ms),
                    server_handling: Duration::ZERO,
                },
            ),
            start,
            end,
        );
    }
    let near = measurements.hosts["near"].accumulator.snapshot();
    let far = measurements.hosts["far"].accumulator.snapshot();
    assert_eq!(near.count, 2);
    assert_eq!(far.count, 2);
    assert_eq!(near.jitter, Some(2_000_000));
    assert_eq!(far.jitter, Some(20_000_000));
    measurements.observe(("near".into(), Observation::ConnectionBoundary), start, end);
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
    measurements.sample(&mut snapshot, Duration::from_millis(500));
    assert_eq!(snapshot.server_latencies[0].latest_ms, Some(4.0));
    assert_eq!(snapshot.server_latencies[1].latest_ms, Some(220.0));
    measurements.sample(&mut snapshot, Duration::from_secs(1));
    assert_eq!(snapshot.server_latencies[0].latest_ms, None);
    assert_eq!(snapshot.server_latencies[1].latest_ms, None);
}

#[tokio::test]
async fn latency_failure_preserves_payload_and_throughput_failure_stops_only_its_latency() {
    let (stop, _) = watch::channel(false);
    let (near_stop, near_cancelled) = watch::channel(false);
    let (far_stop, far_cancelled) = watch::channel(false);
    let mut resources = StageResources {
        transfers: ["near", "far"]
            .into_iter()
            .map(|id| Transfer {
                id: id.into(),
                down: None,
                up: None,
            })
            .collect(),
        latency: JoinSet::new(),
        stop,
        stop_latency: BTreeMap::from([("near".into(), near_stop), ("far".into(), far_stop)]),
        retired: JoinSet::new(),
        failed: Vec::new(),
        latency_failed: false,
    };
    let (snapshots, observed) = watch::channel(Snapshot {
        server_latencies: ["near", "far"]
            .into_iter()
            .map(|id| ServerLatency {
                id: id.into(),
                ..ServerLatency::default()
            })
            .collect(),
        ..Snapshot::default()
    });
    let mut accounting = AggregateMeasurements::default();
    let epoch = Instant::now();
    resources
        .recover(
            LatencyFailure {
                id: "near".into(),
                source: "latency socket closed".into(),
            }
            .into(),
            &mut accounting,
            Some(TransferStage::Bidirectional),
            true,
            epoch,
            &snapshots,
        )
        .unwrap();
    assert_eq!(resources.transfers.len(), 2);
    assert!(*near_cancelled.borrow());
    assert!(!*far_cancelled.borrow());
    assert!(observed.borrow().server_latencies[0].error.is_some());
    assert!(observed.borrow().server_latencies[1].error.is_none());
    resources
        .recover(
            ParticipantFailure {
                id: "far".into(),
                source: "payload disconnected".into(),
            }
            .into(),
            &mut accounting,
            Some(TransferStage::Bidirectional),
            true,
            epoch,
            &snapshots,
        )
        .unwrap();
    assert_eq!(resources.transfers.len(), 1);
    assert_eq!(resources.transfers[0].id, "near");
    assert!(*far_cancelled.borrow());
    resources.close().await.unwrap();
}

#[tokio::test]
async fn loaded_latency_failure_during_warmup_keeps_throughput_participant() {
    let (stop, _) = watch::channel(false);
    let (latency_stop, stopped) = watch::channel(false);
    let mut resources = StageResources {
        transfers: vec![Transfer {
            id: "near".into(),
            down: None,
            up: None,
        }],
        latency: JoinSet::new(),
        stop,
        stop_latency: BTreeMap::from([("near".into(), latency_stop)]),
        retired: JoinSet::new(),
        failed: Vec::new(),
        latency_failed: false,
    };
    let (snapshots, observed) = watch::channel(Snapshot {
        server_latencies: vec![ServerLatency {
            id: "near".into(),
            ..ServerLatency::default()
        }],
        ..Snapshot::default()
    });
    let mut accounting = AggregateMeasurements::default();
    resources
        .recover(
            LatencyFailure {
                id: "near".into(),
                source: "latency socket closed".into(),
            }
            .into(),
            &mut accounting,
            Some(TransferStage::Download),
            false,
            Instant::now(),
            &snapshots,
        )
        .unwrap();
    assert_eq!(resources.transfers[0].id, "near");
    assert!(resources.failed.is_empty());
    assert!(resources.latency_failed);
    assert!(*stopped.borrow());
    assert!(observed.borrow().server_latencies[0].error.is_some());
    assert!(accounting.intervals().is_empty());
    resources.close().await.unwrap();
}

#[test]
fn requested_stop_does_not_hide_a_latency_error() {
    assert!(latency_task_result("near".into(), Ok(()), true).is_ok());
    let error =
        latency_task_result("near".into(), Err("observation queue full".into()), true).unwrap_err();
    let failure = error.downcast::<LatencyFailure>().unwrap();
    assert_eq!(failure.id, "near");
    assert_eq!(failure.source.to_string(), "observation queue full");
    assert!(latency_task_result("far".into(), Ok(()), false).is_err());
}

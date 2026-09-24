//! The unchanged Go product server is started by rust/tests/client_interop.py.
use graphite_meter_client::{
    Error,
    config::Config,
    model::{Phase, Snapshot, Stage},
    net::Http,
    runner,
};
use graphite_meter_core::discovery::{LatencyTransport, Protocol, ThroughputTransport};
use std::time::Duration;
use tokio::sync::watch;

#[tokio::test]
async fn go_server_completes_native_webtransport_stages() -> Result<(), Error> {
    let Ok(url) = std::env::var("GM_GO_INTEROP_URL") else {
        return Ok(());
    };
    let config = Config {
        url,
        stages: vec![
            Stage::Latency,
            Stage::Download,
            Stage::Upload,
            Stage::Bidirectional,
        ],
        throughput_protocol: Some(Protocol::Http3),
        throughput_transport: Some(ThroughputTransport::WebTransport),
        latency_transport: Some(LatencyTransport::WebTransport),
        warmup: Duration::from_millis(100),
        latency_duration: Duration::from_millis(600),
        download_duration: Duration::from_secs(1),
        upload_duration: Duration::from_secs(1),
        bidirectional_duration: Duration::from_secs(1),
        streams: 1,
        loaded_latency: true,
        // The disposable loopback server uses a fresh self-signed certificate.
        insecure: true,
        ..Config::default()
    };
    let (snapshots, _snapshot_rx) = watch::channel(Snapshot::default());
    let (cancel_tx, cancel) = watch::channel(false);
    let _ = graphite_meter_client::crypto::provider().install_default();
    let http = Http::new(config.insecure)?;
    tokio::time::timeout(
        Duration::from_secs(40),
        runner::run(config, http, snapshots.clone(), cancel),
    )
    .await??;
    drop(cancel_tx);

    let snapshot = snapshots.borrow();
    assert_eq!(snapshot.phase, Phase::Complete, "{}", snapshot.status);
    assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
    assert!(snapshot.servers.iter().any(|server| {
        server.throughput.as_ref().is_some_and(|target| {
            target.protocol == Protocol::Http3
                && target.transport == ThroughputTransport::WebTransport
        }) && server
            .latency
            .as_ref()
            .is_some_and(|target| target.transport == LatencyTransport::WebTransport)
    }));
    assert_eq!(snapshot.results.len(), 4);
    for (result, stage) in snapshot.results.iter().zip([
        Stage::Latency,
        Stage::Download,
        Stage::Upload,
        Stage::Bidirectional,
    ]) {
        assert_eq!(result.stage, stage);
        assert!(result.complete, "{} stage remained partial", stage.name());
        if stage.downloads() || stage.uploads() {
            assert!(!result.server_results.is_empty());
        }
        assert!(
            result
                .server_results
                .iter()
                .all(|server| server.error.is_none()),
            "{} stage has a failed server",
            stage.name()
        );
        if stage.downloads() {
            assert!(
                result.down_bytes > 0,
                "{} received no download",
                stage.name()
            );
        }
        if stage.uploads() {
            assert!(result.up_bytes > 0, "{} received no upload", stage.name());
        }
    }
    println!("Rust client completed all four WebTransport stages against Go server");
    Ok(())
}

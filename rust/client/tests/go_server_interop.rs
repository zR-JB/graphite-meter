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

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    protocol: Protocol,
    throughput: ThroughputTransport,
    latency: LatencyTransport,
}

#[tokio::test]
async fn go_server_completes_native_transport_stages() -> Result<(), Error> {
    let Ok(url) = std::env::var("GM_GO_INTEROP_URL") else {
        return Ok(());
    };
    let _ = graphite_meter_client::crypto::provider().install_default();
    for case in [
        Case {
            name: "WebTransport stream",
            protocol: Protocol::Http3,
            throughput: ThroughputTransport::WebTransport,
            latency: LatencyTransport::WebTransport,
        },
        Case {
            name: "WebTransport datagram",
            protocol: Protocol::Http3,
            throughput: ThroughputTransport::WebTransportDatagram,
            latency: LatencyTransport::WebTransport,
        },
        Case {
            name: "HTTPS HTTP/1.1 fetch stream",
            protocol: Protocol::Http1,
            throughput: ThroughputTransport::FetchStream,
            latency: LatencyTransport::WebSocket,
        },
        Case {
            name: "HTTP/2 fetch stream",
            protocol: Protocol::Http2,
            throughput: ThroughputTransport::FetchStream,
            latency: LatencyTransport::WebSocket,
        },
    ] {
        run_case(&url, case).await?;
    }
    Ok(())
}

async fn run_case(url: &str, case: Case) -> Result<(), Error> {
    let config = Config {
        url: url.into(),
        stages: vec![
            Stage::Latency,
            Stage::Download,
            Stage::Upload,
            Stage::Bidirectional,
        ],
        throughput_protocol: Some(case.protocol),
        throughput_transport: Some(case.throughput),
        latency_transport: Some(case.latency),
        warmup: Duration::from_millis(100),
        latency_duration: Duration::from_millis(600),
        download_duration: Duration::from_secs(1),
        // Exercise the reported fetch-upload boundary at the TUI's default
        // duration; the other transports keep this CI replay short.
        upload_duration: if case.protocol == Protocol::Http1 {
            Config::default().upload_duration
        } else {
            Duration::from_secs(1)
        },
        bidirectional_duration: Duration::from_secs(1),
        streams: 1,
        loaded_latency: true,
        insecure: false,
        ..Config::default()
    };
    let (snapshots, _snapshot_rx) = watch::channel(Snapshot::default());
    let (cancel_tx, cancel) = watch::channel(false);
    let http = Http::new(config.insecure)?;
    tokio::time::timeout(
        Duration::from_secs(40),
        runner::run(config, http, snapshots.clone(), cancel),
    )
    .await??;
    drop(cancel_tx);

    let snapshot = snapshots.borrow();
    assert_eq!(
        snapshot.phase,
        Phase::Complete,
        "{}: {}",
        case.name,
        snapshot.status
    );
    assert!(
        snapshot.error.is_none(),
        "{}: {:?}",
        case.name,
        snapshot.error
    );
    assert!(snapshot.servers.iter().any(|server| {
        server.throughput.as_ref().is_some_and(|target| {
            target.protocol == case.protocol && target.transport == case.throughput
        }) && server
            .latency
            .as_ref()
            .is_some_and(|target| target.transport == case.latency)
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
    println!(
        "Rust client completed all four stages using {} against Go server",
        case.name
    );
    Ok(())
}

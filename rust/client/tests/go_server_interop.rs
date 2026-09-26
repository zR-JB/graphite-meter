//! The unchanged Go product server is started by rust/tests/client_interop.py.
use graphite_meter_client::{
    Error,
    config::Config,
    model::{Phase, Snapshot, Stage},
    net::{AuthRequired, Http},
    runner,
};
use graphite_meter_core::{
    catalog::ServerEntry,
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;
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
    run_case_with_http(url, case, Http::new(false)?).await
}

#[tokio::test]
async fn go_server_completes_approved_native_stages() -> Result<(), Error> {
    let Ok(url) = std::env::var("GM_GO_AUTH_URL") else {
        return Ok(());
    };
    let _ = graphite_meter_client::crypto::provider().install_default();
    let http = Http::new(false)?;
    let entry = ServerEntry {
        id: "self".into(),
        url: url.clone(),
        name: "authenticated Go server".into(),
        ..ServerEntry::default()
    };
    let challenge = http.preflight(&entry).await.unwrap_err();
    let required = challenge.downcast::<AuthRequired>()?;
    let pending = http.begin_authorization(&required.origin, &required.login_url)?;
    let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing Rust workspace directory")?
        .join("tests/approve_native.py");
    let mut approval = tokio::process::Command::new("python3")
        .arg(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let input =
        serde_json::to_vec(&[&url, &pending.browser_url, &std::env::var("SSL_CERT_FILE")?])?;
    approval
        .stdin
        .take()
        .ok_or("missing approval input")?
        .write_all(&input)
        .await?;
    let approved = approval.wait_with_output().await?;
    if !approved.status.success() {
        return Err(format!(
            "browser approval fixture failed: {}",
            String::from_utf8_lossy(&approved.stderr)
        )
        .into());
    }
    http.poll_authorization(pending).await?;
    for case in [
        Case {
            name: "approved WebTransport stream",
            protocol: Protocol::Http3,
            throughput: ThroughputTransport::WebTransport,
            latency: LatencyTransport::WebTransport,
        },
        Case {
            name: "approved HTTPS HTTP/1.1 fetch stream",
            protocol: Protocol::Http1,
            throughput: ThroughputTransport::FetchStream,
            latency: LatencyTransport::WebSocket,
        },
    ] {
        run_case_with_http(&url, case, http.clone()).await?;
    }
    Ok(())
}

async fn run_case_with_http(url: &str, case: Case, http: Http) -> Result<(), Error> {
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

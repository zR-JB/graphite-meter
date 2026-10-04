//! The unchanged Go product server is started by rust/tests/client_interop.py.
use graphite_meter_client::{
    Error,
    config::Config,
    failure::Failure,
    model::{Phase, Snapshot, Stage},
    net::Http,
    runner,
};
use graphite_meter_core::{
    catalog::ServerEntry,
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
};
use std::{process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

const STAGES: [Stage; 4] = [Stage::Latency, Stage::Download, Stage::Upload, Stage::Bidirectional];

#[tokio::test]
#[ignore = "requires Go server fixture; run rust/tests/client_interop.py"]
async fn go_server_completes_native_transport_stages() -> Result<(), Error> {
    let url = std::env::var("GM_GO_INTEROP_URL")
        .map_err(|_| "GM_GO_INTEROP_URL is required; run rust/tests/client_interop.py")?;
    let _ = graphite_meter_client::crypto::provider().install_default();
    for (name, protocol) in [
        ("WebTransport stream", Protocol::Http3),
        ("HTTPS HTTP/1.1 fetch stream", Protocol::Http1),
        ("HTTP/2 fetch stream", Protocol::Http2),
    ] {
        run_case(&url, name, protocol, Http::new(false)?).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires authenticated Go server fixture; run rust/tests/client_interop.py"]
async fn go_server_completes_approved_native_stages() -> Result<(), Error> {
    let url =
        std::env::var("GM_GO_AUTH_URL").map_err(|_| "GM_GO_AUTH_URL is required; run rust/tests/client_interop.py")?;
    let _ = graphite_meter_client::crypto::provider().install_default();
    let http = Http::new(false)?;
    let entry = ServerEntry {
        id: "self".into(),
        url: url.clone(),
        name: "authenticated Go server".into(),
        ..ServerEntry::default()
    };
    let challenge = http.preflight(&entry).await.unwrap_err();
    let Failure::SignIn { origin, login_url } = *challenge.downcast::<Failure>()? else {
        return Err("the server did not ask for sign-in".into());
    };
    let pending = http.begin_authorization(&origin, &login_url)?;
    let helper = concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/approve_native.py");
    let mut approval = tokio::process::Command::new("python3")
        .arg(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let input = serde_json::to_vec(&[&url, &pending.browser_url, &std::env::var("SSL_CERT_FILE")?])?;
    let mut stdin = approval.stdin.take().ok_or("missing approval input")?;
    stdin.write_all(&input).await?;
    drop(stdin);
    let approved = approval.wait_with_output().await?;
    if !approved.status.success() {
        return Err(format!("browser approval fixture failed: {}", String::from_utf8_lossy(&approved.stderr)).into());
    }
    http.poll_authorization(pending).await?;
    for (name, protocol) in [
        ("approved WebTransport stream", Protocol::Http3),
        ("approved HTTPS HTTP/1.1 fetch stream", Protocol::Http1),
    ] {
        run_case(&url, name, protocol, http.clone()).await?;
    }
    Ok(())
}

async fn run_case(url: &str, name: &str, protocol: Protocol, http: Http) -> Result<(), Error> {
    // HTTP/3 measures over WebTransport, HTTP/1.1 and HTTP/2 over fetch streams and WebSocket.
    let (throughput, latency) = match protocol {
        Protocol::Http3 => (ThroughputTransport::WebTransport, LatencyTransport::WebTransport),
        _ => (ThroughputTransport::FetchStream, LatencyTransport::WebSocket),
    };
    let config = Config {
        url: url.into(),
        stages: STAGES.to_vec(),
        throughput_protocol: Some(protocol),
        throughput_transport: Some(throughput),
        latency_transport: Some(latency),
        warmup: Duration::from_millis(100),
        latency_duration: Duration::from_secs(1),
        // Allow checkpoint/scheduling delay beyond the 800ms evidence minimum in hosted CI.
        download_duration: Duration::from_secs(3),
        // Exercise the reported fetch-upload boundary at the TUI's default
        // duration; the other transports keep this CI replay short.
        upload_duration: if protocol == Protocol::Http1 {
            Config::default().upload_duration
        } else {
            Duration::from_secs(3)
        },
        bidirectional_duration: Duration::from_secs(3),
        streams: 1,
        loaded_latency: true,
        insecure: false,
        ..Config::default()
    };
    let (snapshots, _snapshot_rx) = watch::channel(Snapshot::default());
    let (cancel_tx, cancel) = watch::channel(false);
    tokio::time::timeout(Duration::from_secs(40), runner::run(config, http, snapshots.clone(), cancel, None)).await??;
    drop(cancel_tx);

    let snapshot = snapshots.borrow();
    assert_eq!(snapshot.phase, Phase::Complete, "{name}: {snapshot:#?}");
    assert!(snapshot.error.is_none(), "{name}: {:?}", snapshot.error);
    assert!(snapshot.servers.iter().any(|server| {
        let measured = server.throughput.as_ref().map(|path| (path.protocol, path.transport));
        let probed = server.latency.as_ref().map(|path| path.transport);
        measured == Some((protocol, throughput)) && probed == Some(latency)
    }));
    assert_eq!(snapshot.results.len(), 4);
    for (result, stage) in snapshot.results.iter().zip(STAGES) {
        assert_eq!(result.stage, stage);
        assert_eq!(
            snapshot.stage_status(result),
            graphite_meter_client::model::StageStatus::Complete,
            "{} stage remained partial: {:?}",
            stage.name(),
            snapshot.failures
        );
        if stage.downloads() || stage.uploads() {
            assert!(!result.server_results.is_empty());
        }
        if stage.downloads() {
            assert!(result.down_bytes() > 0, "{} received no download", stage.name());
        }
        if stage.uploads() {
            assert!(result.up_bytes() > 0, "{} received no upload", stage.name());
        }
    }
    println!("Rust client completed all four stages using {name} against Go server");
    Ok(())
}

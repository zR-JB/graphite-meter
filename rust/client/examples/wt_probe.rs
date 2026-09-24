use graphite_meter_client::{Error, webtransport::Session};
use std::{sync::Arc, time::Duration};
#[tokio::main]
async fn main() -> Result<(), Error> {
    let _ = graphite_meter_client::crypto::provider().install_default();
    let origin = std::env::args()
        .nth(1)
        .ok_or("usage: wt_probe HTTPS_ORIGIN")?;
    tokio::time::timeout(Duration::from_secs(30), run(&origin)).await??;
    if let Some(websocket_origin) = std::env::args().nth(2) {
        validate_websocket(&websocket_origin).await?;
    }
    Ok(())
}

async fn run(origin: &str) -> Result<(), Error> {
    for query in ["bytes=0", "bytes=0&datagrams=1"] {
        let readiness = Session::connect(
            http::Request::get(format!("{origin}/wt/download?{query}")).body(())?,
            true,
            Duration::from_secs(3),
        )
        .await?;
        readiness.close().await;
        println!("zero-byte download CONNECT {query} PASS");
    }
    let ping = Session::connect(
        http::Request::get(format!("{origin}/wt/ping")).body(())?,
        true,
        Duration::from_secs(5),
    )
    .await?;
    ping.send_ping(42).await?;
    let pong = tokio::time::timeout(Duration::from_secs(3), ping.recv_pong()).await??;
    assert_eq!(pong.id, 42);
    ping.close().await;
    println!("ping PASS");
    for suffix in ["bytes=4096", "bytes=4096&datagrams=1"] {
        let session = Session::connect(
            http::Request::get(format!("{origin}/wt/download?{suffix}")).body(())?,
            true,
            Duration::from_secs(5),
        )
        .await?;
        if suffix.contains("datagrams") {
            assert!(
                !tokio::time::timeout(Duration::from_secs(3), session.recv_datagram())
                    .await??
                    .is_empty()
            );
        } else {
            for _ in 0..2 {
                let mut stream =
                    tokio::time::timeout(Duration::from_secs(3), session.accept_uni()).await??;
                let mut count = 0;
                while let Some(chunk) = stream.read_chunk().await? {
                    count += chunk.len();
                }
                assert_eq!(count, 4096);
            }
        }
        session.close().await;
        println!("download {suffix} PASS");
    }
    let http = Arc::new(
        graphite_meter_client::quic::Http3Client::connect(
            &format!("{origin}/").parse()?,
            true,
            Duration::from_secs(5),
        )
        .await?,
    );
    let mut mint = http
        .open(
            http::Request::post(format!("{origin}/upload/session")).body(())?,
            Default::default(),
        )
        .await?;
    mint.finish().await?;
    assert!(mint.response().await?.status().is_success());
    let value: serde_json::Value = serde_json::from_slice(&mint.recv_body().await?)?;
    let id = value["uploadId"].as_str().ok_or("missing upload ID")?;
    drop(mint);
    let session = Session::connect(
        http::Request::get(format!("{origin}/wt/upload?id={id}")).body(())?,
        true,
        Duration::from_secs(5),
    )
    .await?;
    let mut progress = session.upload_progress().await?;
    assert_eq!(
        progress.next().await?,
        graphite_meter_core::wire::UploadProgress::Ready
    );
    let mut lane = session.open_uni().await?;
    lane.write_chunk(bytes::Bytes::from(vec![b'w'; 131073]))
        .await?;
    lane.finish()?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                progress.next().await?,
                graphite_meter_core::wire::UploadProgress::Progress { bytes: 131073, .. }
            ) {
                break;
            }
        }
        Ok::<_, Error>(())
    })
    .await??;
    let mut finish = http
        .open(
            http::Request::delete(format!("{origin}/upload/progress?id={id}")).body(())?,
            Default::default(),
        )
        .await?;
    finish.finish().await?;
    assert!(finish.response().await?.status().is_success());
    finish.recv_body().await?;
    drop(finish);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                progress.next().await?,
                graphite_meter_core::wire::UploadProgress::Complete { bytes: 131073, .. }
            ) {
                break;
            }
        }
        Ok::<_, Error>(())
    })
    .await??;
    session.close().await;
    Arc::try_unwrap(http)
        .map_err(|_| "HTTP/3 request stream retained its owner")?
        .close()
        .await;
    println!("upload ready/progress/complete131073 PASS");
    validate_owners(origin).await?;
    Ok(())
}

async fn validate_owners(origin: &str) -> Result<(), Error> {
    use graphite_meter_client::{
        download::Download, latency::Observation, net::Http, webtransport,
    };
    use graphite_meter_core::discovery::{Protocol, ThroughputTarget, ThroughputTransport};
    let http = Http::new(true)?;
    let (_cancel, cancelled) = tokio::sync::watch::channel(false);
    let (observations, mut received) = tokio::sync::mpsc::channel(64);
    webtransport::run_latency(
        &http,
        origin,
        true,
        Duration::from_millis(20),
        Duration::from_millis(300),
        observations,
        cancelled.clone(),
    )
    .await?;
    let mut replies = 0;
    while let Some(observation) = received.recv().await {
        if matches!(observation, Observation::Sample { .. }) {
            replies += 1;
        }
    }
    assert!(replies > 0, "WT latency owner produced no replies");
    println!("WT latency owner PASS");
    for transport in [
        ThroughputTransport::WebTransport,
        ThroughputTransport::WebTransportDatagram,
    ] {
        let target = ThroughputTarget {
            base_url: origin.to_owned(),
            transport,
            protocol: Protocol::Http3,
        };
        let mut download = Download::start_webtransport(
            &http,
            &target,
            2,
            Duration::from_secs(5),
            true,
            cancelled.clone(),
        )
        .await?;
        assert!(download.bytes() > 0);
        download.health()?;
        tokio::time::timeout(Duration::from_secs(2), download.stop()).await?;
        println!("WT download owner {transport:?} readiness/accounting/shutdown PASS");
    }
    Ok(())
}

async fn validate_websocket(origin: &str) -> Result<(), Error> {
    use graphite_meter_client::{latency, net::Http};
    let http = Http::new(true)?;
    let (_cancel, cancelled) = tokio::sync::watch::channel(false);
    let (observations, mut received) = tokio::sync::mpsc::channel(64);
    latency::run(
        &http,
        origin,
        true,
        Duration::from_millis(20),
        Duration::from_millis(300),
        observations,
        cancelled,
    )
    .await?;
    let mut replies = 0;
    while let Some(observation) = received.recv().await {
        if matches!(observation, latency::Observation::Sample { .. }) {
            replies += 1;
        }
    }
    assert!(
        replies > 0,
        "shared WebSocket latency loop produced no replies"
    );
    println!("shared WebSocket latency owner PASS");
    Ok(())
}

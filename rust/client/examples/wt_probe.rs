use graphite_meter_client::{Error, webtransport::Session};
use std::time::Duration;
#[tokio::main]
async fn main() -> Result<(), Error> {
    let origin = std::env::args().nth(1).unwrap();
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
            let mut stream =
                tokio::time::timeout(Duration::from_secs(3), session.accept_uni()).await??;
            assert!(!stream.read_chunk().await?.unwrap().is_empty());
        }
        session.close().await;
        println!("download {suffix} PASS");
    }
    let http = graphite_meter_client::quic::Http3Client::connect(
        &format!("{origin}/").parse()?,
        true,
        Duration::from_secs(5),
    )
    .await?;
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
    http.close().await;
    println!("upload ready/progress/complete131073 PASS");
    Ok(())
}

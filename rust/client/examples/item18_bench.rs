//! Measurement-only H3 transfer driver: one fixed transfer per QUIC connection.
use bytes::Bytes;
use graphite_meter_client::{Error, net::Http, transport::Transport};
use graphite_meter_core::{discovery::Protocol, route::Route};
use http::Method;
use std::{sync::Arc, time::{Duration, Instant}};

#[tokio::main]
async fn main() -> Result<(), Error> {
    graphite_meter_client::crypto::provider()
        .install_default()
        .map_err(|_| "TLS provider already installed")?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let [origin, mode, connections, bytes] = &args[..] else {
        return Err("usage: item18_bench ORIGIN download|upload CONNECTIONS BYTES".into());
    };
    let connections: usize = connections.parse()?;
    let bytes: u64 = bytes.parse()?;
    let mut transports = Vec::new();
    for _ in 0..connections {
        let transport =
            Transport::connect(Http::new(true)?, origin, Protocol::Http3, true).await?;
        let _: serde_json::Value = transport.json(Method::GET, Route::Probe, &[]).await?;
        transports.push(Arc::new(transport));
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Minted {
        upload_id: String,
    }
    // Upload sessions belong to the minting client address, so each connection mints its own.
    let mut ids = Vec::new();
    for transport in &transports {
        ids.push(if mode == "upload" {
            let minted: Minted = transport.json(Method::POST, Route::UploadSession, &[]).await?;
            minted.upload_id
        } else {
            String::new()
        });
    }
    let mut block = vec![0_u8; 64 * 1024];
    getrandom::fill(&mut block).map_err(|_| "randomness unavailable")?;
    let block = Bytes::from(block);
    let started = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for (lane, (transport, id)) in transports.into_iter().zip(ids).enumerate() {
        let (mode, block) = (mode.clone(), block.clone());
        tasks.spawn(async move {
            if mode == "download" {
                let size = bytes.to_string();
                let mut body = transport
                    .receive(Method::GET, Route::Download, &[("bytes", &size)], bytes, Duration::from_secs(300))
                    .await?;
                let mut received = 0;
                while let Some(chunk) = body.chunk().await? {
                    received += chunk.len() as u64;
                }
                if received != bytes {
                    return Err::<(), Error>("short download".into());
                }
            } else {
                let body = futures_util::stream::unfold(bytes, move |remaining| {
                    let block = block.clone();
                    async move {
                        (remaining > 0).then(|| {
                            let size = remaining.min(block.len() as u64);
                            (Ok::<_, Error>(block.slice(..size as usize)), remaining - size)
                        })
                    }
                });
                let lane = lane.to_string();
                transport
                    .send(Route::Upload, &[("id", &id), ("lane", &lane)], body, bytes, Duration::from_secs(300))
                    .await?;
            }
            Ok(())
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.map_err(|_| "transfer task failed")??;
    }
    let seconds = started.elapsed().as_secs_f64();
    let total = bytes * connections as u64;
    println!(
        "{}",
        serde_json::json!({"mode":mode,"connections":connections,"bytes":total,"seconds":seconds,"mbps":total as f64*8.0/seconds/1e6})
    );
    Ok(())
}

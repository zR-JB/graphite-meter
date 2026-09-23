//! Transport-only benchmark driver; excludes TUI rendering and stage accounting.
use graphite_meter_client::{Error, net::Http, transport::Transport};
use graphite_meter_core::{
    discovery::{Probe, Protocol},
    route::Route,
};
use http::Method;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), Error> {
    graphite_meter_client::crypto::provider()
        .install_default()
        .map_err(|_| "TLS provider already installed")?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if !(2..=3).contains(&args.len()) || args.get(2).is_some_and(|arg| arg != "--insecure") {
        return Err("usage: http_bench ORIGIN http1|http2|http3 [--insecure]".into());
    }
    let protocol = match args[1].as_str() {
        "http1" => Protocol::Http1,
        "http2" => Protocol::Http2,
        "http3" => Protocol::Http3,
        _ => return Err("unknown protocol".into()),
    };
    let insecure = args.len() == 3;
    let transport = Transport::connect(Http::new(insecure)?, &args[0], protocol, insecure).await?;
    let probe: Probe = transport.json(Method::GET, Route::Probe, &[]).await?;
    probe.validate()?;
    let started = Instant::now();
    let mut bytes = 0_u64;
    let mut requests = 0_u64;
    const SIZE: u64 = 256 * 1024 * 1024;
    while started.elapsed() < Duration::from_secs(3) {
        let mut body = transport
            .receive(
                Method::GET,
                Route::Download,
                &[("bytes", "268435456")],
                SIZE,
                Duration::from_secs(30),
            )
            .await?;
        let mut received = 0;
        while let Some(chunk) = body.chunk().await? {
            received += chunk.len() as u64;
        }
        if received != SIZE {
            return Err("download ended before requested byte count".into());
        }
        bytes += received;
        requests += 1;
    }
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "{}",
        serde_json::json!({"bytes":bytes,"requests":requests,"seconds":seconds,"gbps":bytes as f64*8.0/seconds/1e9,"protocol":probe.protocol_negotiated})
    );
    Ok(())
}

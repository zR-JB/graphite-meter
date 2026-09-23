#![forbid(unsafe_code)]

include!(concat!(env!("OUT_DIR"), "/legal.rs"));

use graphite_meter_client::{
    Error,
    cli::{self, Action},
    controller,
    model::Snapshot,
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("graphite-meter Rust client: {}", safe(&error.to_string()));
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Error> {
    let config = match cli::parse(std::env::args_os().skip(1))? {
        Action::Help => {
            print!("{}", cli::HELP);
            return Ok(());
        }
        Action::Version => {
            println!(
                "{}",
                option_env!("GM_ENGINE_VERSION")
                    .unwrap_or(concat!(env!("CARGO_PKG_VERSION"), "-rust-dev"))
            );
            return Ok(());
        }
        Action::Legal => {
            let report = LEGAL.ok_or("this development build has no reviewed Rust dependency notice bundle; build with GM_RUST_LEGAL_DIR to embed generated notices")?;
            print!("{report}");
            return Ok(());
        }
        Action::Run(config) => *config,
    };
    let _ = graphite_meter_client::crypto::provider().install_default();
    #[cfg(unix)]
    let snapshot = {
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        controller::run(config, async move {
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
            }
        })
        .await?
    };
    #[cfg(not(unix))]
    let snapshot = controller::run(config, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    report(&snapshot);
    Ok(())
}
fn report(snapshot: &Snapshot) {
    println!("Graphite Meter · {}", safe(&snapshot.status));
    for server in snapshot
        .servers
        .iter()
        .filter(|server| !server.transport.is_empty())
    {
        println!(
            "{} · {} · {}",
            safe(&server.name),
            safe(&server.origin),
            safe(&server.transport)
        );
        if let Some(error) = &server.error {
            println!("  Unavailable: {}", safe(error));
        }
    }
    for result in &snapshot.results {
        println!(
            "{}{}: down {}, up {}, received down {} bytes / up {} bytes",
            result.stage.name(),
            if result.complete { "" } else { " (partial)" },
            rate(result.down_bps),
            rate(result.up_bps),
            result.down_bytes,
            result.up_bytes
        );
        for host in &result.server_latencies {
            let p50 = host.summary.distribution.map_or_else(
                || "unavailable".into(),
                |distribution| format!("{:.3} ms", distribution.p50 as f64 / 1_000_000.0),
            );
            println!(
                "  {}: RTT p50 {}, replies {}, timeouts {}, unresolved {}",
                safe(&host.id),
                p50,
                host.summary.count,
                host.summary.timeouts,
                host.summary.unresolved
            );
            if let Some(error) = &host.error {
                println!("    Latency unavailable: {}", safe(error));
            }
        }
        if result.server_results.len() > 1 {
            for server in &result.server_results {
                println!(
                    "  {}: down {}, up {}, received down {} bytes / up {} bytes",
                    safe(&server.id),
                    rate(server.down_bps),
                    rate(server.up_bps),
                    server.down_bytes,
                    server.up_bytes,
                );
                if let Some(error) = &server.error {
                    println!("    Unavailable: {}", safe(error));
                }
            }
        }
    }
    if let Some(error) = &snapshot.error {
        println!("Error: {}", safe(error));
    }
}
fn rate(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map_or_else(
            || "unavailable".into(),
            |value| format!("{:.3} Mbit/s", value / 1_000_000.0),
        )
}
fn safe(value: &str) -> String {
    value
        .chars()
        .take(4096)
        .map(|character| {
            let formatting_control = matches!(
                character,
                '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}' | '\u{feff}'
            );
            if character.is_control() || formatting_control {
                '�'
            } else {
                character
            }
        })
        .collect()
}

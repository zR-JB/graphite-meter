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
    match run().await {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("graphite-meter Rust client: {}", safe(&error.to_string()));
            std::process::exit(1);
        }
    }
}
async fn run() -> Result<i32, Error> {
    let (config, report_only) = match cli::parse(std::env::args_os().skip(1))? {
        Action::Help => {
            print!("{}", cli::HELP);
            return Ok(0);
        }
        Action::Version => {
            println!(
                "{}",
                option_env!("GM_ENGINE_VERSION")
                    .unwrap_or(concat!(env!("CARGO_PKG_VERSION"), "-rust-dev"))
            );
            return Ok(0);
        }
        Action::Legal => {
            let (compressed, length) = LEGAL.ok_or("this development build has no reviewed Rust dependency notice bundle; build with GM_RUST_LEGAL_DIR to embed generated notices")?;
            let report =
                miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(compressed, length)
                    .map_err(|_| "embedded Rust legal notices are corrupt")?;
            if report.len() != length {
                return Err("embedded Rust legal notice length mismatch".into());
            }
            use std::io::Write;
            std::io::stdout().lock().write_all(&report)?;
            return Ok(0);
        }
        Action::Run { config, report } => (*config, report),
    };
    let _ = graphite_meter_client::crypto::provider().install_default();
    use std::io::IsTerminal;
    use std::sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    };
    let caught = Arc::new(AtomicU8::new(0));
    let signal = caught.clone();
    #[cfg(unix)]
    let shutdown = {
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        async move {
            let code = tokio::select! { _ = interrupt.recv() => 130, _ = terminate.recv() => 143 };
            signal.store(code, Ordering::Relaxed);
        }
    };
    #[cfg(not(unix))]
    let shutdown = async move {
        let _ = tokio::signal::ctrl_c().await;
        signal.store(130, Ordering::Relaxed);
    };
    let snapshot = if report_only || !std::io::stdout().is_terminal() {
        controller::run_once(config, shutdown).await?
    } else {
        controller::run(config, shutdown).await?
    };
    report(&snapshot);
    let signal = caught.load(Ordering::Relaxed);
    Ok(if signal != 0 {
        i32::from(signal)
    } else {
        match snapshot.phase {
            graphite_meter_client::model::Phase::Complete
            | graphite_meter_client::model::Phase::Setup => 0,
            graphite_meter_client::model::Phase::Cancelled => 1,
            _ => 1,
        }
    })
}

fn report(snapshot: &Snapshot) {
    println!("Graphite Meter · {}", safe(&snapshot.status));
    for server in snapshot
        .servers
        .iter()
        .filter(|server| server.has_check_result())
    {
        println!(
            "{} · {} · {}",
            safe(&server.name),
            safe(&server.origin),
            if server.checked() {
                safe(&server.connection_label())
            } else {
                "Unavailable".into()
            }
        );
        if let Some(error) = &server.error {
            println!("  Unavailable: {}", safe(error));
        }
    }
    for result in &snapshot.results {
        let ending = if result.complete {
            ""
        } else if result.elapsed.is_zero() {
            " · Failed"
        } else {
            " · Partial"
        };
        if result.stage == graphite_meter_client::model::Stage::Latency {
            println!("{}{ending}", result.stage.name());
        } else {
            println!(
                "{}{ending}: {} {}, {} {}",
                result.stage.name(),
                graphite_meter_client::vocabulary::DOWNLOAD.label,
                rate(result.down_bps()),
                graphite_meter_client::vocabulary::UPLOAD.label,
                rate(result.up_bps())
            );
            for measurement in [&result.down, &result.up].into_iter().flatten() {
                let direction =
                    if measurement.direction == graphite_meter_core::measurement::Direction::Down {
                        graphite_meter_client::vocabulary::DOWNLOAD
                    } else {
                        graphite_meter_client::vocabulary::UPLOAD
                    };
                println!(
                    "  {} · {}",
                    direction.label,
                    graphite_meter_client::vocabulary::throughput_facts(measurement)
                );
            }
        }
        for host in &result.server_latencies {
            let name = snapshot
                .servers
                .iter()
                .find(|server| server.id == host.id)
                .map_or(host.id.as_str(), |server| server.name.as_str());
            let median = host.median().map_or_else(
                || "—".into(),
                |value| {
                    format!(
                        "{} ms",
                        graphite_meter_core::format::latency_ms(value as f64 / 1e6)
                    )
                },
            );
            let timeouts = host
                .summary
                .timeout_ratio()
                .map_or_else(|| "—".into(), |value| format!("{:.0}%", value * 100.0));
            let milliseconds = |value: Option<u64>| {
                value.map_or_else(
                    || "—".into(),
                    |value| {
                        format!(
                            "{} ms",
                            graphite_meter_core::format::latency_ms(value as f64 / 1e6)
                        )
                    },
                )
            };
            let added = if result.stage == graphite_meter_client::model::Stage::Latency {
                "—".into()
            } else {
                snapshot
                    .results
                    .iter()
                    .find(|result| result.stage == graphite_meter_client::model::Stage::Latency)
                    .and_then(|result| {
                        result
                            .server_latencies
                            .iter()
                            .find(|idle| idle.id == host.id)
                    })
                    .and_then(|idle| Some((host.median()?, idle.median()?)))
                    .map_or_else(
                        || "—".into(),
                        |(loaded, idle)| {
                            format!(
                                "{} ms",
                                graphite_meter_core::format::added_ms(
                                    (loaded as f64 - idle as f64) / 1e6
                                )
                            )
                        },
                    )
            };
            use graphite_meter_client::vocabulary as words;
            println!(
                "  {}: {} {}, {} {}, {} {}, {} {}, {} {}, replies {}, unfinished probes {}",
                safe(name),
                words::MEDIAN.label,
                median,
                words::ADDED.label,
                added,
                words::P95.label,
                milliseconds(host.summary.distribution.map(|value| value.p95)),
                words::JITTER.label,
                milliseconds(host.summary.jitter),
                words::PROBE_TIMEOUTS.label,
                timeouts,
                host.summary.count,
                host.summary.unresolved
            );
            if let Some(error) = &host.error {
                println!("    Latency unavailable: {}", safe(error));
            }
        }
        if result.server_results.len() > 1 {
            for server in &result.server_results {
                println!(
                    "  {}: Download {}, Upload {}, received Download {} / Upload {}",
                    safe(
                        snapshot
                            .servers
                            .iter()
                            .find(|host| host.id == server.id)
                            .map_or(server.id.as_str(), |host| host.name.as_str())
                    ),
                    rate(server.down_bps()),
                    rate(server.up_bps()),
                    graphite_meter_core::format::bytes(server.down_bytes()),
                    graphite_meter_core::format::bytes(server.up_bytes()),
                );
                if let Some(error) = &server.error {
                    println!("    Unavailable: {}", safe(error));
                }
            }
        }
    }
    if !snapshot.failures.is_empty() {
        println!("Left the test");
        for failure in &snapshot.failures {
            let name = snapshot
                .servers
                .iter()
                .find(|host| host.id == failure.server_id)
                .map_or(failure.server_id.as_str(), |host| host.name.as_str());
            println!(
                "  {} · {} · {}",
                safe(name),
                failure.stage.name(),
                failure.reason.label()
            );
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
            || "—".into(),
            |value| graphite_meter_core::format::rate(value / 8.0),
        )
}
fn safe(value: &str) -> String {
    value
        .chars()
        .take(4096)
        .map(|character| {
            if !graphite_meter_core::text::terminal_character(character) {
                '�'
            } else {
                character
            }
        })
        .collect()
}

#![forbid(unsafe_code)]

mod password_command;

use graphite_meter_server::{
    ServerError,
    config::{self, ValidatedConfig},
    runtime,
};

#[tokio::main]
async fn main() {
    // A development build's plain marker, which release verification refuses, stays in the executable.
    std::hint::black_box(graphite_meter_server::assets::DEVELOPMENT_NOTICES);
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    // Go's fatal log lines, which alerting keys on.
    let failure = match args.as_slice() {
        [only] if only == "version" || only == "--version" => {
            println!("{}", config::ENGINE_VERSION);
            None
        }
        [only] if only == "--legal" || only == "-legal" => match legal() {
            Ok(0) => None,
            Ok(status) => std::process::exit(status),
            Err(error) => Some(format!("legal: {error}")),
        },
        [only] if only == "hash-password" => password_command::run()
            .err()
            .map(|error| format!("hash-password: {error}")),
        _ => match config::load(|name| std::env::var_os(name), &args, &mut std::io::stderr()) {
            Ok(None) => None,
            Ok(Some(config)) => serve(config)
                .await
                .err()
                .map(|error| format!("server error: {:?}", error.to_string())),
            Err(error) => Some(format!("configuration error: {:?}", error.to_string())),
        },
    };
    if let Some(failure) = failure {
        graphite_meter_server::log!("{failure}");
        std::process::exit(1);
    }
}

fn legal() -> Result<i32, ServerError> {
    let report = graphite_meter_server::assets::legal_report()
        .ok_or("this build embeds no notices; mise run rust-server-run -- --legal builds the server with reviewed host notices and prints them")?;
    Ok(print(std::io::stdout().lock(), report)?)
}

/// Writes `report` and returns the exit status. Into a closed pipe it ends quietly, as a Go program does: it dies
/// of SIGPIPE on Unix, which shells report as 141, and ignores the failed write elsewhere.
fn print(mut out: impl std::io::Write, report: &[u8]) -> std::io::Result<i32> {
    match out.write_all(report).and_then(|()| out.flush()) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(if cfg!(unix) { 141 } else { 0 }),
        written => written.map(|()| 0),
    }
}

async fn serve(config: ValidatedConfig) -> Result<(), ServerError> {
    // Register handlers before binding sockets, so a signal during startup is
    // retained and causes shutdown as soon as startup completes.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        runtime::run(config, async move {
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
            }
        })
        .await
    }
    #[cfg(not(unix))]
    {
        runtime::run(config, async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                graphite_meter_server::log!("shutdown signal listener failed: {error}");
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn notices_into_a_closed_pipe_end_quietly_as_go_output_does() -> std::io::Result<()> {
        let (reader, writer) = std::io::pipe()?;
        drop(reader);
        assert_eq!(super::print(writer, b"notices\n")?, if cfg!(unix) { 141 } else { 0 });
        Ok(())
    }
}

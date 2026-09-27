#![forbid(unsafe_code)]

mod password_command;

use graphite_meter_server::{
    config::{self, Config, ConfigError},
    runtime,
};

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    // Go's fatal log lines, which alerting keys on.
    let failure = match args.as_slice() {
        [only] if only == "version" || only == "--version" => {
            println!("{}", config::ENGINE_VERSION);
            None
        }
        [only] if only == "--legal" || only == "-legal" => legal().err().map(|error| format!("legal: {error}")),
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

fn legal() -> Result<(), ConfigError> {
    use std::io::Write;
    let report = graphite_meter_server::assets::legal_report()
        .ok_or("this development build has no reviewed Rust dependency notice bundle")?;
    std::io::stdout().lock().write_all(report)?;
    Ok(())
}

async fn serve(config: Config) -> Result<(), ConfigError> {
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

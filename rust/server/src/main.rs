#![forbid(unsafe_code)]

include!(concat!(env!("OUT_DIR"), "/legal.rs"));

mod password_command;

use graphite_meter_server::{
    cli::{self, Arguments},
    config::{Config, ConfigError},
    runtime,
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("graphite-meter Rust: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), ConfigError> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "version" || args[0] == "--version") {
        println!("{}", graphite_meter_server::config::ENGINE_VERSION);
        return Ok(());
    }
    if args.len() == 1 && (args[0] == "--legal" || args[0] == "-legal") {
        let report =
            LEGAL.ok_or("this development build has no reviewed Rust dependency notice bundle")?;
        use std::io::Write;
        let mut output = std::io::stdout().lock();
        output.write_all(report.as_bytes())?;
        if LEGAL_USES_BROWSER_NOTICES {
            let notices = graphite_meter_server::assets::legal_notices()
                .ok_or("reviewed browser notices are missing")?;
            output.write_all(notices)?;
        }
        return Ok(());
    }
    if args.len() == 1 && args[0] == "hash-password" {
        password_command::run()?;
        return Ok(());
    }
    let config = match cli::parse(&args)? {
        Arguments::Help => {
            print!("{}", cli::help());
            return Ok(());
        }
        Arguments::Overrides(overrides) => Config::load_with_overrides(&overrides)?,
    };
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
                eprintln!("shutdown signal listener failed: {error}");
            }
        })
        .await
    }
}

//! The Graphite Meter native terminal client.

use graphite_meter_client::{
    INTERRUPTED, TERMINATED, VERSION,
    config::{self, Config, Parsed, Refusal},
    events::View,
    headless, report, status, text as styled,
    tui::{
        self,
        theme::{self, Palette},
    },
};
use graphite_meter_net::Pool;
use graphite_meter_proto::text;
use std::{
    io::{self, IsTerminal},
    process::ExitCode,
    sync::Arc,
};
use tokio::sync::mpsc::{self, UnboundedReceiver};

include!(concat!(env!("OUT_DIR"), "/legal.rs"));

/// The status of a refused command line.
const USAGE_ERROR: u8 = 2;

fn main() -> ExitCode {
    NOTICES.keep();
    let mut args = std::env::args_os();
    let program = args.next().unwrap_or_default();
    let usage = || config::usage(&text::clean(&program.to_string_lossy(), usize::MAX, text::safe));
    match config::parse(args) {
        Ok(Parsed::Help) => eprint!("{}", usage()),
        Ok(Parsed::Version) => println!("graphite-meter-client {VERSION}"),
        Ok(Parsed::Legal) => return legal(),
        Ok(Parsed::Run(config)) => return run(*config).unwrap_or_else(|error| fail(&error.to_string())),
        Err(Refusal::Flag(error)) => {
            eprint!("{error}\n{}", usage());
            return ExitCode::from(USAGE_ERROR);
        }
        Err(Refusal::Argument(error)) => {
            eprintln!("graphite-meter-client: {error}");
            return ExitCode::from(USAGE_ERROR);
        }
    }
    ExitCode::SUCCESS
}

fn fail(error: &str) -> ExitCode {
    eprintln!("graphite-meter-client: {error}");
    ExitCode::FAILURE
}

/// Runs the interface on a terminal and prints the report of the finished run it shows; otherwise, or with `-report`,
/// runs `config` once and prints its report to stdout, or why it never started to stderr.
fn run(config: Config) -> io::Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let pool = Arc::new(runtime.block_on(async { Pool::new() })?);
    runtime.block_on(async {
        let terminal = io::stdout().is_terminal();
        let interactive = terminal && !config.report;
        let signals = signals(!interactive)?;
        if interactive {
            let exit = tui::interactive(config, pool, signals).await?;
            if exit.report {
                print(&exit.view, &exit.palette, columns(terminal), terminal)?;
            }
            return Ok(ExitCode::from(status(&exit.view, exit.signal)));
        }
        let columns = columns(terminal);
        #[cfg(unix)]
        let dark = match columns.is_some() && io::stdin().is_terminal() {
            true => theme::background(std::time::Duration::from_secs(2)).await,
            false => None,
        };
        #[cfg(not(unix))]
        let dark = None;
        let (view, status) = headless(config, pool, first(signals)).await;
        match report::unstarted(&view) {
            Some(reason) => eprintln!("graphite-meter-client: {reason}"),
            None => print(&view, &Palette::new(dark.unwrap_or(true)), columns, terminal)?,
        }
        Ok(ExitCode::from(status))
    })
}

/// The terminal's width, when stdout is one.
fn columns(terminal: bool) -> Option<u16> {
    let columns = crossterm::terminal::size().ok().map(|(columns, _)| columns);
    columns.filter(|&columns| terminal && columns > 0)
}

/// Writes `view`'s report to stdout as wide as the terminal, or 100 columns.
fn print(view: &View, palette: &Palette, columns: Option<u16>, terminal: bool) -> io::Result<()> {
    let width = columns.map_or(report::WIDTH, |columns| usize::from(columns).max(40));
    let lines = report::report(view, width, palette);
    let profile = theme::profile(terminal, |name| std::env::var(name).ok());
    styled::write(&lines, profile, &mut io::stdout())
}

/// The first signal's status.
async fn first(mut signals: UnboundedReceiver<u8>) -> u8 {
    match signals.recv().await {
        Some(status) => status,
        None => std::future::pending().await,
    }
}

/// Each interrupt's and termination's status as it arrives; with `at_once`, a second exits at once with its own.
fn signals(at_once: bool) -> io::Result<UnboundedReceiver<u8>> {
    let (caught, signals) = mpsc::unbounded_channel();
    let mut first = true;
    let mut interrupt = move |status: u8| {
        if at_once && !std::mem::take(&mut first) {
            std::process::exit(status.into());
        }
        let _ = caught.send(status);
    };
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (mut interrupts, mut terminations) = (signal(SignalKind::interrupt())?, signal(SignalKind::terminate())?);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    Some(()) = interrupts.recv() => interrupt(INTERRUPTED),
                    Some(()) = terminations.recv() => interrupt(TERMINATED),
                    else => break,
                }
            }
        });
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};
        let (mut interrupts, mut breaks, mut closes) = (ctrl_c()?, ctrl_break()?, ctrl_close()?);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    Some(()) = interrupts.recv() => interrupt(INTERRUPTED),
                    Some(()) = breaks.recv() => interrupt(INTERRUPTED),
                    Some(()) = closes.recv() => interrupt(TERMINATED),
                    else => break,
                }
            }
        });
    }
    Ok(signals)
}

/// Prints the notices; the status is 141 into a closed pipe on Unix.
fn legal() -> ExitCode {
    let printed = NOTICES
        .report()
        .ok_or_else(|| {
            "this build embeds no notices; mise run rust-client-run -- --legal builds the TUI with dependency notices \
             and prints them"
                .to_owned()
        })
        .and_then(|report| graphite_meter_legal::print(io::stdout().lock(), report).map_err(|error| error.to_string()));
    match printed {
        Ok(status) => ExitCode::from(status),
        Err(error) => fail(&error),
    }
}

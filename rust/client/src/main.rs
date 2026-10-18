use graphite_meter_client::{
    INTERRUPTED, TERMINATED, VERSION,
    config::{self, Config, Parsed, Refusal},
    events::View,
    headless, report, status,
    style::{self, Palette},
    text as styled, tui,
};
use graphite_meter_net::Pool;
use graphite_meter_proto::{origin::Origin, text};
use std::{
    io::{self, IsTerminal},
    process::ExitCode,
    sync::Arc,
};
use tokio::sync::mpsc::{self, UnboundedReceiver};

include!(concat!(env!("OUT_DIR"), "/legal.rs"));

#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: rustfs_mimalloc::MiMalloc = rustfs_mimalloc::MiMalloc;

const USAGE_ERROR: u8 = 2;

fn main() -> ExitCode {
    NOTICES.keep();
    let mut args = std::env::args_os();
    let program = args.next().unwrap_or_default();
    let usage = || config::usage(&text::clean(&program.to_string_lossy(), usize::MAX));
    match config::parse(args) {
        Ok(Parsed::Help) => eprint!("{}", usage()),
        Ok(Parsed::Version) => {
            return written(format!("graphite-meter-client {VERSION}\n").as_bytes());
        }
        Ok(Parsed::Legal) => return legal(),
        Ok(Parsed::Run(mut config)) => {
            let kept = config::kept_server().and_then(|path| config::recall(&path));
            if config.url.is_none() {
                config.url.clone_from(&kept);
            }
            let interactive = io::stdout().is_terminal() && !config.report;
            if config.url.is_none() && !interactive {
                eprintln!(
                    "graphite-meter-client: no server address: pass --url with your Graphite Meter server, for example \
                     https://meter.example"
                );
                return ExitCode::from(USAGE_ERROR);
            }
            return run(*config, kept, interactive)
                .unwrap_or_else(|error| fail(&error.to_string()));
        }
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

/// The interface when `interactive`, else one run of `config` and its report; `kept` is the remembered server.
fn run(config: Config, kept: Option<Origin>, interactive: bool) -> io::Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let pool = Arc::new(Pool::probing(runtime.handle())?);
    runtime.block_on(async {
        let terminal = io::stdout().is_terminal();
        let signals = signals(!interactive)?;
        if interactive {
            let exit = tui::interactive(config, kept, pool, signals).await?;
            let status = status(&exit.view, exit.signal);
            if exit.report {
                return print(
                    &exit.view,
                    &exit.palette,
                    columns(terminal),
                    terminal,
                    status,
                );
            }
            return Ok(ExitCode::from(status));
        }
        let columns = columns(terminal);
        let asked = match columns.is_some() && io::stdin().is_terminal() {
            true => Some(tokio::task::spawn_blocking(|| match style::Raw::enter() {
                Ok(_raw) => style::ask(style::ANSWER),
                Err(_) => style::Answer::default(),
            })),
            false => None,
        };
        let answer = match asked {
            Some(asked) => asked.await.unwrap_or_default(),
            None => style::Answer::default(),
        };
        let stop = async {
            match answer.interrupted {
                true => INTERRUPTED,
                false => first(signals).await,
            }
        };
        let (view, status) = headless(config, pool, stop).await;
        if let Some(reason) = report::unreported(&view) {
            eprintln!("graphite-meter-client: {reason}");
            return Ok(ExitCode::from(status));
        }
        print(
            &view,
            &answer
                .background
                .map_or_else(|| Palette::new(true), Palette::over),
            columns,
            terminal,
            status,
        )
    })
}

fn columns(terminal: bool) -> Option<u16> {
    let columns = crossterm::terminal::size().ok().map(|(columns, _)| columns);
    columns.filter(|&columns| terminal && columns > 0)
}

/// Writes `view`'s report to stdout at terminal width or 100 columns, ending with `status`; 141 into a closed pipe.
fn print(
    view: &View,
    palette: &Palette,
    columns: Option<u16>,
    terminal: bool,
    status: u8,
) -> io::Result<ExitCode> {
    let width = columns.map_or(report::WIDTH, |columns| usize::from(columns).max(40));
    let lines = report::report(view, width, palette);
    let (profile, mut out) = (
        style::profile(terminal, |name| std::env::var(name).ok()),
        Vec::new(),
    );
    styled::write(&lines, profile, &mut out)?;
    let printed = graphite_meter_legal::print(io::stdout().lock(), &out)?;
    Ok(ExitCode::from(if printed == 0 { status } else { printed }))
}

async fn first(mut signals: UnboundedReceiver<u8>) -> u8 {
    match signals.recv().await {
        Some(status) => status,
        None => std::future::pending().await,
    }
}

/// Each interrupt's and termination's status; with `at_once`, a second exits at once, the terminal put back first.
fn signals(at_once: bool) -> io::Result<UnboundedReceiver<u8>> {
    let (caught, signals) = mpsc::unbounded_channel();
    let mut stopping = false;
    let mut interrupt = move |status: u8| {
        if std::mem::replace(&mut stopping, true) && at_once {
            let _ = crossterm::terminal::disable_raw_mode();
            std::process::exit(status.into());
        }
        let _ = caught.send(status);
    };
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (mut interrupts, mut terminations) = (
            signal(SignalKind::interrupt())?,
            signal(SignalKind::terminate())?,
        );
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

fn legal() -> ExitCode {
    match NOTICES.report() {
        Some(report) => written(report),
        None => fail(
            "this build embeds no notices; mise run tui -- --legal builds the client with dependency notices and \
             prints them",
        ),
    }
}

/// Writes `bytes` to stdout; the status is 141 into a closed pipe on Unix.
fn written(bytes: &[u8]) -> ExitCode {
    match graphite_meter_legal::print(io::stdout().lock(), bytes) {
        Ok(status) => ExitCode::from(status),
        Err(error) => fail(&error.to_string()),
    }
}

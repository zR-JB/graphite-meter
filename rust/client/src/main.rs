#![forbid(unsafe_code)]

include!(concat!(env!("OUT_DIR"), "/legal.rs"));

use crossterm::terminal::size as terminal_size;
use graphite_meter_client::{
    Error,
    cli::{self, Action},
    controller,
    model::Phase,
    report::{self, safe},
};
use std::{
    io::IsTerminal,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
use tokio::sync::mpsc;

// musl's malloc re-faults freed memory, which cut HTTP/3 download throughput by 40%.
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

#[tokio::main]
async fn main() {
    // A development build's plain marker, which release verification refuses, stays in the executable.
    std::hint::black_box(DEVELOPMENT_NOTICES);
    // Go's usage names the program as it was invoked.
    let program = std::env::args_os().next().unwrap_or_default();
    let usage = cli::usage(&safe(&program.to_string_lossy()));
    let code = match cli::parse(std::env::args_os().skip(1)) {
        Ok(Action::Help) => {
            eprint!("{usage}");
            0
        }
        Ok(Action::Version) => {
            println!("graphite-meter-client {}", graphite_meter_client::VERSION);
            0
        }
        Ok(Action::Legal) => legal().unwrap_or_else(|error| fail(&error, 1)),
        Ok(Action::Run { config, report }) => run(*config, report).await.unwrap_or_else(|error| fail(&error, 1)),
        // Go's flag package prints its refusal without the program name, then the usage.
        Err(error) if error.is::<cli::FlagError>() => {
            eprint!("{}\n{usage}", safe(&error.to_string()));
            2
        }
        Err(error) => fail(&error, 2),
    };
    if code != 0 {
        std::process::exit(code);
    }
}
fn fail(error: &Error, code: i32) -> i32 {
    eprintln!("graphite-meter-client: {}", safe(&error.to_string()));
    code
}
fn legal() -> Result<i32, Error> {
    let (compressed, length) = LEGAL.ok_or("this build embeds no notices; mise run rust-client-run -- --legal builds the TUI with reviewed host notices and prints them")?;
    let report = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(compressed, length)
        .map_err(|_| "embedded Rust legal notices are corrupt")?;
    if report.len() != length {
        return Err("embedded Rust legal notice length mismatch".into());
    }
    Ok(print(std::io::stdout().lock(), &report)?)
}
/// Writes `report` and returns the exit status. Into a closed pipe it ends quietly with Go's: Go dies of
/// SIGPIPE on Unix, which shells report as 141, and ignores the failed write elsewhere.
fn print(mut out: impl std::io::Write, report: &[u8]) -> std::io::Result<i32> {
    match out.write_all(report).and_then(|()| out.flush()) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(if cfg!(unix) { 141 } else { 0 }),
        written => written.map(|()| 0),
    }
}
async fn run(config: graphite_meter_client::config::Config, report_only: bool) -> Result<i32, Error> {
    let _ = graphite_meter_client::crypto::provider().install_default();
    let terminal = std::io::stdout().is_terminal();
    let headless = report_only || !terminal;
    // The handlers come first, as Go's: a signal during the query stops the run once the terminal is restored.
    let caught = Arc::new(AtomicU8::new(0));
    let interrupts = interrupts(headless, caught.clone())?;
    let columns = || terminal_size().ok().filter(|(columns, _)| terminal && *columns > 0);
    if headless && columns().is_some() && std::io::stdin().is_terminal() {
        // As Go's runHeadless asks lipgloss.HasDarkBackground before the run, when stdout has a width.
        crossterm::terminal::enable_raw_mode()?;
        report::ask_background().await;
        crossterm::terminal::disable_raw_mode()?;
    }
    let (finished, exit) = match headless {
        true => (
            Some(controller::run_once(config, interrupts).await?),
            Default::default(),
        ),
        false => controller::run(config, interrupts).await?,
    };
    let width = columns().map_or(report::WIDTH, |(columns, _)| usize::from(columns).max(40));
    // Go prints the run its view shows, which a return to setup leaves none of.
    let shown = if headless { &finished } else { &exit.shown };
    match shown.as_ref().and_then(|snapshot| report::print(snapshot, width)) {
        Some(report) => println!("{report}"),
        None if headless => {
            let error = finished.as_ref().and_then(|snapshot| snapshot.error.as_deref());
            fail(&error.unwrap_or("Test stopped before it started.").into(), 1);
        }
        None => {}
    }
    let last = finished.map(|snapshot| snapshot.phase);
    let signal = caught.load(Ordering::Relaxed);
    let interrupted = exit.interrupted || signal != 0 && (exit.running || last == Some(Phase::Cancelled));
    Ok(if interrupted {
        if signal == 143 { 143 } else { 130 }
    } else if last.is_none_or(|phase| phase == Phase::Complete) {
        0
    } else {
        1
    })
}

fn interrupts(headless: bool, caught: Arc<AtomicU8>) -> Result<mpsc::Receiver<()>, Error> {
    let (sender, receiver) = mpsc::channel(4);
    let interrupt = move |code: u8| {
        if caught.swap(code, Ordering::Relaxed) != 0 && headless {
            std::process::exit(code.into());
        }
        let _ = sender.try_send(());
    };
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupts = signal(SignalKind::interrupt())?;
        let mut terminations = signal(SignalKind::terminate())?;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = interrupts.recv() => interrupt(130),
                    _ = terminations.recv() => interrupt(143),
                }
            }
        });
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        let (mut interrupts, mut breaks, mut closes) =
            (windows::ctrl_c()?, windows::ctrl_break()?, windows::ctrl_close()?);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = interrupts.recv() => interrupt(130),
                    _ = breaks.recv() => interrupt(130),
                    _ = closes.recv() => interrupt(143),
                }
            }
        });
    }
    Ok(receiver)
}

#[cfg(test)]
mod tests {
    #[test]
    fn notices_into_a_closed_pipe_end_quietly_with_go_status() -> std::io::Result<()> {
        let (reader, writer) = std::io::pipe()?;
        drop(reader);
        assert_eq!(super::print(writer, b"notices\n")?, if cfg!(unix) { 141 } else { 0 });
        Ok(())
    }
}

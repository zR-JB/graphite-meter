#![forbid(unsafe_code)]

include!(concat!(env!("OUT_DIR"), "/legal.rs"));

use graphite_meter_client::{
    Error,
    cli::{self, Action},
    controller,
    model::Phase,
    report, ui,
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
    let code = match cli::parse(std::env::args_os().skip(1)) {
        Ok(action) => run(action).await.unwrap_or_else(|error| fail(&error, 1)),
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
async fn run(action: Action) -> Result<i32, Error> {
    let (config, report_only) = match action {
        Action::Help => {
            print!("{}", cli::HELP);
            return Ok(0);
        }
        Action::Version => {
            println!(
                "{}",
                option_env!("GM_ENGINE_VERSION").unwrap_or(concat!(env!("CARGO_PKG_VERSION"), "-rust-dev"))
            );
            return Ok(0);
        }
        Action::Legal => {
            let (compressed, length) = LEGAL.ok_or("this development build has no reviewed Rust dependency notice bundle; build with GM_RUST_LEGAL_DIR to embed generated notices")?;
            let report = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(compressed, length)
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
    let headless = report_only || !std::io::stdout().is_terminal();
    let caught = Arc::new(AtomicU8::new(0));
    let interrupts = interrupts(headless, caught.clone())?;
    let (finished, exit) = if headless {
        (
            Some(controller::run_once(config, interrupts).await?),
            ui::Exit::default(),
        )
    } else {
        controller::run(config, interrupts).await?
    };
    let width = crossterm::terminal::size()
        .ok()
        .filter(|_| std::io::stdout().is_terminal())
        .map_or(report::WIDTH, |(columns, _)| usize::from(columns).max(40));
    if let Some(snapshot) = finished.as_ref().filter(|_| !exit.running) {
        match report::render(snapshot, width) {
            Some(report) => println!("{}", report.lines().map(safe).collect::<Vec<_>>().join("\n")),
            None if headless => eprintln!(
                "graphite-meter-client: {}",
                snapshot.error.as_deref().unwrap_or("Test stopped before it started.")
            ),
            None => {}
        }
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

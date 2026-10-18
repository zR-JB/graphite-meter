//! Runs the binary. It handles `version`, `hash-password` and `--legal`, or serves as the environment and flags say.

use graphite_meter_net::Pool;
use graphite_meter_server::{
    assets::NOTICES,
    auth::password,
    config::{self, Config, ENGINE_VERSION, Loaded},
    log,
    runtime::{self, Server},
};
use std::{
    io::{self, BufRead, IsTerminal, Read, Write},
    process::ExitCode,
};
use zeroize::Zeroizing;

#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: rustfs_mimalloc::MiMalloc = rustfs_mimalloc::MiMalloc;

fn main() -> ExitCode {
    NOTICES.keep();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let failure = match args.as_slice() {
        [only] if only == "version" || only == "--version" => {
            println!("{ENGINE_VERSION}");
            None
        }
        [only] if only == "--legal" || only == "-legal" => match legal() {
            Ok(0) => None,
            Ok(status) => return ExitCode::from(status),
            Err(error) => Some(("legal", error)),
        },
        [only] if only == "hash-password" => hash_password().err().map(|error| ("auth", error)),
        _ => match config::load(|name| std::env::var_os(name), args, &mut io::stderr()) {
            Ok(Loaded::Help) => None,
            Ok(Loaded::Config(config)) => {
                if let Some(zone) = &config.log_zone {
                    log::show_in(zone.clone());
                }
                match runtime::check_budget(&config) {
                    Ok(()) => serve(*config).err().map(|error| ("server", error)),
                    Err(error) => Some(("config", error)),
                }
            }
            Err(error) => Some(("config", error)),
        },
    };
    let status = match failure {
        // A failure's lines log one after another, so a long one never wraps.
        Some((topic, failure)) => {
            for line in failure.lines() {
                log!(Error, topic, "{line}");
            }
            ExitCode::FAILURE
        }
        None => ExitCode::SUCCESS,
    };
    log::flush();
    status
}

/// Prints the notices. The status is 141 into a closed pipe on Unix.
fn legal() -> Result<u8, String> {
    let report = NOTICES.report().ok_or(
        "this build embeds no notices; mise run dev -- --legal builds the server with dependency notices \
         and prints them",
    )?;
    graphite_meter_legal::print(io::stdout().lock(), report)
        .map_err(|error| format!("could not print the notices: {error}"))
}

/// Runs the server until SIGINT or SIGTERM.
fn serve(config: Config) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start the runtime: {error}"))?;
    let pool = Pool::beside(runtime.handle())
        .map_err(|error| format!("could not start the runtime threads: {error}"))?;
    runtime.block_on(async {
        let stop = runtime::stop_signal()
            .map_err(|error| format!("could not watch for stop signals: {error}"))?;
        Server::bind(config, pool).await?.serve(stop).await
    })
}

/// Reads a password twice, without echo on a terminal, and prints its hash.
fn hash_password() -> Result<(), String> {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    // The unix arm's guard lives to the end of the function, as cfg_select! expands an arm's statements in place.
    cfg_select! {
        unix => {
            let _hidden = terminal
                .then(|| Hidden::new(io::stdin()))
                .transpose()
                .map_err(|error| format!("could not hide the terminal input: {error}"))?;
        }
        _ => {
            if terminal {
                return Err("hidden terminal input is unsupported on this platform".into());
            }
        }
    }
    let mut input = stdin.lock();
    let mut entry = |prompt: &str| -> io::Result<Zeroizing<Vec<u8>>> {
        let mut prompts = io::stderr().lock();
        write!(prompts, "{prompt}")?;
        prompts.flush()?;
        let line = read_line(&mut input)?;
        if terminal {
            writeln!(prompts)?;
        }
        Ok(line)
    };
    let unread = |error: io::Error| format!("could not read the password: {error}");
    let first = entry("Password: ").map_err(unread)?;
    password::validate(&first)?;
    let second = entry("Confirm password: ").map_err(unread)?;
    if first != second {
        return Err("passwords do not match".into());
    }
    let hash = password::hash(&first)?;
    println!("{hash}");
    Ok(())
}

/// Reads a line of up to a password's bytes and its line break, and drops trailing line breaks.
fn read_line(input: &mut impl BufRead) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut line = Zeroizing::new(Vec::with_capacity(LINE_BYTES));
    input
        .by_ref()
        .take(LINE_BYTES as u64)
        .read_until(b'\n', &mut line)?;
    let last = line.iter().rposition(|byte| !matches!(byte, b'\r' | b'\n'));
    line.truncate(last.map_or(0, |last| last + 1));
    Ok(line)
}

const LINE_BYTES: usize = password::MAX_BYTES + 2;

/// Turns terminal echo off until dropped.
#[cfg(unix)]
struct Hidden {
    terminal: io::Stdin,
    original: rustix::termios::Termios,
}

#[cfg(unix)]
impl Hidden {
    fn new(terminal: io::Stdin) -> io::Result<Self> {
        use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};
        let original = tcgetattr(&terminal)?;
        let mut hidden = original.clone();
        hidden
            .local_modes
            .remove(LocalModes::ECHO | LocalModes::ECHONL);
        tcsetattr(&terminal, OptionalActions::Now, &hidden)?;
        Ok(Self { terminal, original })
    }
}

#[cfg(unix)]
impl Drop for Hidden {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(
            &self.terminal,
            rustix::termios::OptionalActions::Now,
            &self.original,
        );
    }
}

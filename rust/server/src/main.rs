//! The server binary: `version`, `hash-password`, `--legal`, or the server configured by environment and flags.

use graphite_meter_proto::text::quote;
use graphite_meter_server::{
    assets::NOTICES,
    auth::password,
    config::{self, Config, ENGINE_VERSION, Loaded},
    log,
    runtime::{self, Server},
};
use std::{
    io::{self, BufRead, IsTerminal, Write},
    process::ExitCode,
};
use zeroize::Zeroizing;

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
            Err(error) => Some(format!("legal: {error}")),
        },
        [only] if only == "hash-password" => hash_password().err().map(|error| format!("hash-password: {error}")),
        _ => match config::load(|name| std::env::var_os(name), args, &mut io::stderr()) {
            Ok(Loaded::Help) => None,
            Ok(Loaded::Config(config)) => serve(*config)
                .err()
                .map(|error| format!("server error: {}", quote(&error))),
            Err(error) => Some(format!("configuration error: {}", quote(&error))),
        },
    };
    match failure {
        Some(failure) => {
            log!("{failure}");
            ExitCode::FAILURE
        }
        None => ExitCode::SUCCESS,
    }
}

/// Prints the notices; the status is 141 into a closed pipe on Unix.
fn legal() -> Result<u8, String> {
    let report = NOTICES.report().ok_or(
        "this build embeds no notices; mise run rust-server-run -- --legal builds the server with dependency notices \
         and prints them",
    )?;
    graphite_meter_legal::print(io::stdout().lock(), report).map_err(|error| error.to_string())
}

/// Runs the server until SIGINT or SIGTERM.
fn serve(config: Config) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let stop = runtime::stop_signal().map_err(|error| error.to_string())?;
        Server::bind(config).await?.serve(stop).await
    })
}

/// Reads a password twice, without echo on a terminal, and prints its hash.
fn hash_password() -> Result<(), String> {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    #[cfg(unix)]
    let _hidden = terminal
        .then(|| Hidden::new(io::stdin()))
        .transpose()
        .map_err(|error| error.to_string())?;
    #[cfg(not(unix))]
    if terminal {
        return Err("hidden terminal input is unsupported on this platform".into());
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
    let first = entry("Password: ").map_err(|error| error.to_string())?;
    let second = entry("Confirm password: ").map_err(|error| error.to_string())?;
    if first != second {
        return Err("passwords do not match".into());
    }
    let hash = password::hash(&first)?;
    println!("{hash}");
    Ok(())
}

/// A line without its trailing line breaks.
fn read_line(input: &mut impl BufRead) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut line = Zeroizing::new(Vec::new());
    input.read_until(b'\n', &mut line)?;
    let kept = line
        .iter()
        .rposition(|byte| !matches!(byte, b'\r' | b'\n'))
        .map_or(0, |last| last + 1);
    line.truncate(kept);
    Ok(line)
}

/// Terminal echo turned off until dropped.
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
        hidden.local_modes.remove(LocalModes::ECHO | LocalModes::ECHONL);
        tcsetattr(&terminal, OptionalActions::Now, &hidden)?;
        Ok(Self { terminal, original })
    }
}

#[cfg(unix)]
impl Drop for Hidden {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(&self.terminal, rustix::termios::OptionalActions::Now, &self.original);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_line_loses_only_its_trailing_breaks() {
        for (input, line) in
            [(&b"secret\r\n"[..], &b"secret"[..]), (b"a\rb\n", b"a\rb"), (b"tail", b"tail"), (b"", b"")]
        {
            assert_eq!(super::read_line(&mut &input[..]).unwrap().as_slice(), line);
        }
    }
}

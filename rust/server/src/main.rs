//! Runs the binary. It handles `version`, `hash-password`, `healthcheck` and `--legal`, or serves as the settings say.

use graphite_meter_net::Pool;
use graphite_meter_proto::duration;
use graphite_meter_server::{
    assets::NOTICES,
    auth::password,
    config::{self, Config, ENGINE_VERSION, ListenerKind, Loaded},
    log,
    runtime::{self, Server},
};
use std::{
    io::{self, BufRead, IsTerminal, Read, Write},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs},
    process::ExitCode,
    time::Duration,
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
        [only] if only == "healthcheck" => healthcheck().err().map(|error| ("health", error)),
        _ => match load(args) {
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

fn load(args: Vec<std::ffi::OsString>) -> Result<Loaded, String> {
    let mut usage = Vec::new();
    let loaded = config::load(|name| std::env::var_os(name), args, &mut usage);
    let _ = match loaded {
        Ok(Loaded::Help) => io::stdout().write_all(&usage),
        _ => io::stderr().write_all(&usage),
    };
    loaded
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

fn healthcheck() -> Result<(), String> {
    const BOUND: Duration = Duration::from_secs(2);
    const REQUEST: &str = "GET /probe HTTP/1.1\r\nHost: localhost\r\nX-Real-IP: 127.0.0.1\r\nConnection: close\r\n\r\n";
    let Loaded::Config(config) = config::load(|name| std::env::var_os(name), [], &mut io::sink())?
    else {
        return Err("the settings name no listener".into());
    };
    if let Some(zone) = &config.log_zone {
        log::show_in(zone.clone());
    }
    let address = &config
        .listener(ListenerKind::H1)
        .expect("the clear listener is required")
        .address;
    let targets: Vec<SocketAddr> = match address.host.as_deref() {
        None | Some("0.0.0.0") => vec![SocketAddr::from((Ipv4Addr::LOCALHOST, address.port))],
        Some("[::]") => vec![SocketAddr::from((Ipv6Addr::LOCALHOST, address.port))],
        Some(_) => address
            .to_string()
            .to_socket_addrs()
            .map_err(|error| config::could_not(&format!("resolve {address}"), &error))?
            .collect(),
    };
    let mut refused = format!("{address} resolves to no address");
    for target in targets {
        let failed = |error: io::Error| match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                format!(
                    "{target} did not answer /probe within {}",
                    duration::format(BOUND)
                )
            }
            io::ErrorKind::UnexpectedEof => {
                format!("{target} closed the connection without answering /probe")
            }
            _ => config::could_not(&format!("ask {target} for /probe"), &error),
        };
        let mut stream = match TcpStream::connect_timeout(&target, BOUND) {
            Ok(stream) => stream,
            Err(error) => {
                refused = failed(error);
                continue;
            }
        };
        stream.set_read_timeout(Some(BOUND)).map_err(failed)?;
        stream.set_write_timeout(Some(BOUND)).map_err(failed)?;
        stream.write_all(REQUEST.as_bytes()).map_err(failed)?;
        let mut head = [0; 12];
        stream.read_exact(&mut head).map_err(failed)?;
        let status = head
            .strip_prefix(b"HTTP/1.1 ")
            .and_then(|code| std::str::from_utf8(code).ok()?.parse::<u16>().ok());
        return match status {
            Some(status) if status < 500 => Ok(()),
            Some(status) => Err(format!("{target} answered /probe with HTTP {status}")),
            None => Err(format!(
                "{target} answered /probe without an HTTP/1.1 status"
            )),
        };
    }
    Err(refused)
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

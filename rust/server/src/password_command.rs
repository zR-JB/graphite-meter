//! Interactive password hashing without exposing a password in process arguments.

use graphite_meter_server::{config::ConfigError, password};
use std::io::{self, BufRead, IsTerminal, Write};
use zeroize::Zeroizing;

const MAX_INPUT_LINE: u64 = 1024 + 2; // Password bytes plus an optional CRLF.

pub fn run() -> Result<(), ConfigError> {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut prompts = io::stderr().lock();
    let mut output = io::stdout().lock();

    #[cfg(unix)]
    let mut echo = terminal.then(|| EchoGuard::hide(&input)).transpose()?;
    #[cfg(not(unix))]
    if terminal {
        return Err("hidden terminal input is unsupported on this platform".into());
    }

    write!(prompts, "Password: ")?;
    prompts.flush()?;
    let first = read_password(&mut input)?;
    if terminal {
        writeln!(prompts)?;
    }
    write!(prompts, "Confirm password: ")?;
    prompts.flush()?;
    let second = read_password(&mut input)?;
    if terminal {
        writeln!(prompts)?;
    }

    #[cfg(unix)]
    if let Some(guard) = &mut echo {
        guard.restore()?;
    }

    if first != second {
        return Err("passwords do not match".into());
    }
    let hash = password::hash_password(&first)?;
    writeln!(output, "{hash}")?;
    Ok(())
}

fn read_password(input: &mut impl BufRead) -> Result<Zeroizing<String>, ConfigError> {
    let mut bytes = Zeroizing::new(Vec::new());
    io::Read::take(input, MAX_INPUT_LINE).read_until(b'\n', &mut bytes)?;
    let line = std::str::from_utf8(&bytes)?;
    let value = line.trim_end_matches(['\r', '\n']);
    password::validate_password(value)?;
    Ok(Zeroizing::new(value.to_owned()))
}

#[cfg(unix)]
struct EchoGuard {
    fd: rustix::fd::OwnedFd,
    original: Option<rustix::termios::Termios>,
}

#[cfg(unix)]
impl EchoGuard {
    fn hide(input: &impl std::os::fd::AsFd) -> io::Result<Self> {
        use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};

        let fd = rustix::io::fcntl_dupfd_cloexec(input, 0)?;
        let original = tcgetattr(&fd)?;
        let mut hidden = original.clone();
        hidden
            .local_modes
            .remove(LocalModes::ECHO | LocalModes::ECHONL);
        tcsetattr(&fd, OptionalActions::Now, &hidden)?;
        Ok(Self {
            fd,
            original: Some(original),
        })
    }

    fn restore(&mut self) -> io::Result<()> {
        use rustix::termios::{OptionalActions, tcsetattr};

        if let Some(original) = &self.original {
            tcsetattr(&self.fd, OptionalActions::Now, original)?;
            self.original = None;
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for EchoGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

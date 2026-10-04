//! Interactive password hashing without exposing a password in process arguments.

use graphite_meter_server::{ServerError, password};
use std::io::{self, BufRead, IsTerminal, Write};
use zeroize::Zeroizing;

pub fn run() -> Result<(), ServerError> {
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
    let first = read_line(&mut input)?;
    if terminal {
        writeln!(prompts)?;
    }
    write!(prompts, "Confirm password: ")?;
    prompts.flush()?;
    let second = read_line(&mut input)?;
    if terminal {
        writeln!(prompts)?;
    }

    #[cfg(unix)]
    if let Some(guard) = &mut echo {
        guard.restore()?;
    }

    // As Go's hashPassword: the entries are compared first, then validated and hashed as bytes.
    if first != second {
        return Err("passwords do not match".into());
    }
    let hash = password::hash_password(&*first)?;
    writeln!(output, "{hash}")?;
    Ok(())
}

/// A line without its trailing line breaks, as Go's ReadString and TrimRight read it.
fn read_line(input: &mut impl BufRead) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut line = Zeroizing::new(Vec::new());
    input.read_until(b'\n', &mut line)?;
    let end = line.iter().rposition(|byte| !matches!(byte, b'\r' | b'\n'));
    line.truncate(end.map_or(0, |end| end + 1));
    Ok(line)
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
        hidden.local_modes.remove(LocalModes::ECHO | LocalModes::ECHONL);
        tcsetattr(&fd, OptionalActions::Now, &hidden)?;
        Ok(Self { fd, original: Some(original) })
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

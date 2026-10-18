//! Drives the binary in a pseudo-terminal through `terminal.py` against a peer that never answers.
#![cfg(unix)]
use std::{io::ErrorKind, net::TcpListener, path::Path, process::Command};

const CLIENT: &str = env!("CARGO_BIN_EXE_graphite-meter-client");
const DRIVER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/terminal.py");

struct Session {
    status: i64,
    /// Counts the mode's keys that were sent.
    keys: i64,
    /// The client wrote this to the terminal.
    text: String,
    /// Measures seconds from the termination to the exit, in the signals mode.
    exited: Option<f64>,
    /// The terminal echoes and reads lines again after the exit.
    restored: bool,
}

/// Drives the client's session in `mode`, none without python3, which CI must have.
fn drive(mode: &str) -> Option<Session> {
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port().to_string();
    let directory = Path::new(CLIENT).parent().unwrap();
    let output = Command::new("python3")
        .arg(DRIVER)
        .args([mode, &port])
        .current_dir(directory)
        .output();
    let output = match output {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            assert!(std::env::var_os("CI").is_none(), "python3 is missing");
            eprintln!("skipped: python3 is missing");
            return None;
        }
        output => output.unwrap(),
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let number = |name: &str| result[name].as_i64().unwrap();
    let (text, exited) = (
        result["text"].as_str().unwrap().to_owned(),
        result["exited"].as_f64(),
    );
    let restored = result["restored"].as_bool().unwrap();
    Some(Session {
        status: number("code"),
        keys: number("keys"),
        text,
        exited,
        restored,
    })
}

/// Picks what the client wrote after leaving the alternate screen.
fn after_restore(session: &Session) -> &str {
    let (_, after) = session
        .text
        .rsplit_once("\x1b[?1049l")
        .expect("the alternate screen closed");
    after
}

#[test]
fn q_at_setup_exits_0() {
    let Some(session) = drive("quit") else { return };
    assert_eq!((session.keys, session.status), (1, 0), "{:?}", session.text);
    assert!(
        session.text.contains("Start"),
        "the setup frame: {:?}",
        session.text
    );
    assert!(
        !session.text.contains("\x1b]9;4"),
        "no progress bar where the terminal draws none"
    );
    assert!(
        session.text.contains("\x1b[22;0t\x1b[?1003h\x1b[?1006h"),
        "title pushed, every move reported"
    );
    assert!(
        session.text.contains("\x1b[?1006l\x1b[?1003l\x1b[23;0t"),
        "title put back"
    );
    assert_eq!(after_restore(&session), "");
    assert!(session.restored);
}

#[test]
fn ctrl_c_during_a_run_exits_130() {
    let Some(session) = drive("interrupt") else {
        return;
    };
    assert_eq!(
        (session.keys, session.status),
        (2, 130),
        "{:?}",
        session.text
    );
    assert!(
        session.text.contains("\x1b]9;4;0\x07"),
        "the progress bar is cleared"
    );
    assert_eq!(
        after_restore(&session),
        "",
        "a test stopped before it started has no report"
    );
    assert!(session.restored);
}

#[test]
fn ctrl_c_while_the_report_asks_the_terminal_stops_with_130() {
    let Some(session) = drive("query") else {
        return;
    };
    assert_eq!(
        (session.keys, session.status),
        (1, 130),
        "{:?}",
        session.text
    );
    assert!(
        session
            .text
            .ends_with("Test stopped before it started.\r\n"),
        "{:?}",
        session.text
    );
    assert!(session.restored);
}

/// The unanswered background query holds the report for 2 s in raw mode, so it cannot exit by itself meanwhile.
#[cfg(target_os = "linux")]
#[test]
fn a_termination_after_an_interrupt_exits_143_at_once_and_restores_the_terminal() {
    let Some(session) = drive("signals") else {
        return;
    };
    assert_eq!(session.status, 143, "{:?}", session.text);
    assert!(session.exited.unwrap() < 2.0, "{:?}", session.exited);
    assert_eq!(
        session.text, "\x1b]11;?\x1b\\\x1b[c",
        "nothing but the background query"
    );
    assert!(session.restored, "the terminal echoes again");
}

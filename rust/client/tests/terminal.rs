//! The binary in a terminal, driven through a pseudo-terminal by `terminal.py` against a peer that never answers.
#![cfg(unix)]
use std::{io::ErrorKind, net::TcpListener, path::Path, process::Command};

const CLIENT: &str = env!("CARGO_BIN_EXE_graphite-meter-client");
const DRIVER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/terminal.py");

struct Session {
    status: i64,
    /// How many of the mode's keys were sent.
    keys: i64,
    /// What the client wrote to the terminal.
    text: String,
}

/// The client's session in `mode`; none without python3.
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
            eprintln!("skipped: python3 is missing");
            return None;
        }
        output => output.unwrap(),
    };
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let number = |name: &str| result[name].as_i64().unwrap();
    let text = result["text"].as_str().unwrap().to_owned();
    Some(Session { status: number("code"), keys: number("keys"), text })
}

/// What the client wrote after leaving the alternate screen.
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
    assert!(session.text.contains("Start"), "the setup frame: {:?}", session.text);
    assert!(session.text.contains("\x1b]9;4;0\x07"));
    assert_eq!(after_restore(&session), "");
}

#[test]
fn ctrl_c_during_a_run_exits_130() {
    let Some(session) = drive("interrupt") else { return };
    assert_eq!((session.keys, session.status), (2, 130), "{:?}", session.text);
    assert_eq!(after_restore(&session), "", "a test stopped before it started has no report");
}

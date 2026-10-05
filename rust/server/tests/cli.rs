use graphite_meter_server::config::ENGINE_VERSION;
use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

fn server(args: &[&str], env: &[(&str, &str)], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
        .args(args)
        .env_clear()
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

/// The message of a log line after its `YYYY/MM/DD HH:MM:SS ` timestamp.
fn logged(line: &str) -> &str {
    let (stamp, message) = line.split_at(20);
    let shape = stamp
        .bytes()
        .map(|byte| if byte.is_ascii_digit() { b'0' } else { byte })
        .collect::<Vec<_>>();
    assert_eq!(shape, b"0000/00/00 00:00:00 ", "{line}");
    message
}

#[test]
fn version_prints_the_engine_version() {
    for arg in ["version", "--version"] {
        let output = server(&[arg], &[], b"");
        assert!(output.status.success());
        assert_eq!(text(&output.stdout), format!("{ENGINE_VERSION}\n"));
    }
}

#[test]
fn configuration_errors_log_go_fatal_line_and_exit_one() {
    let output = server(&[], &[("GM_PUBLIC_ORIGINS", "bad\"origin")], b"");
    assert_eq!(output.status.code(), Some(1));
    let line = text(&output.stderr).strip_suffix('\n').unwrap();
    let message = r#"configuration error: "GM_PUBLIC_ORIGINS contains invalid origin \"bad\\\"origin\"""#;
    assert_eq!(logged(line), message);
}

#[test]
fn help_prints_the_usage_and_exits_zero() {
    let output = server(&["-h"], &[], b"");
    assert!(output.status.success());
    assert_eq!(text(&output.stderr), include_str!("usage.txt"));
    let output = server(&["-nope"], &[], b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("flag provided but not defined: -nope\nUsage:\n"));
}

#[test]
fn legal_prints_nothing_without_embedded_notices() {
    for arg in ["-legal", "--legal"] {
        let output = server(&[arg], &[], b"");
        assert!(output.status.success() && output.stdout.is_empty() && output.stderr.is_empty());
    }
}

#[test]
fn hash_password_reads_twice_from_a_pipe() {
    let output = server(&["hash-password"], &[], b"correct horse\r\ncorrect horse\n");
    assert!(output.status.success());
    assert_eq!(text(&output.stderr), "Password: Confirm password: ");
    assert!(text(&output.stdout).starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    let output = server(&["hash-password"], &[], b"one\ntwo\n");
    assert_eq!(output.status.code(), Some(1));
    let line = text(&output.stderr)
        .strip_prefix("Password: Confirm password: ")
        .unwrap();
    assert_eq!(logged(line.trim_end()), "hash-password: passwords do not match");
}

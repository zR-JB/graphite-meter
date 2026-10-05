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
fn legal_without_embedded_notices_names_the_task_that_embeds_them() {
    for arg in ["-legal", "--legal"] {
        let output = server(&[arg], &[], b"");
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let line = text(&output.stderr).strip_suffix('\n').unwrap();
        let message = "legal: this build embeds no notices; mise run rust-server-run -- --legal builds the server \
                       with dependency notices and prints them";
        assert_eq!(logged(line), message);
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

#[cfg(unix)]
#[test]
fn sigterm_stops_a_serving_server_with_exit_zero() {
    use std::io::{BufRead, BufReader};
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
        .env_clear()
        .env("GM_H1_ADDR", "127.0.0.1:0")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let role = "HTTP/1.1 clear: UI, discovery, probe, transfers, WebSockets";
    assert_eq!(
        logged(line.trim_end()),
        format!("graphite-meter {ENGINE_VERSION} listening on 127.0.0.1:0/tcp ({role})")
    );
    let killed = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
    assert!(killed.unwrap().success());
    assert!(child.wait().unwrap().success());
}

#[test]
fn runtime_failures_log_a_server_error_and_exit_one() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = taken.local_addr().unwrap().to_string();
    let output = server(&[], &[("GM_H1_ADDR", &address)], b"");
    assert_eq!(output.status.code(), Some(1));
    let line = text(&output.stderr).strip_suffix('\n').unwrap();
    assert_eq!(logged(line), format!(r#"server error: "listen tcp {address}: address already in use""#));
}

#[test]
fn a_buffer_budget_below_the_connection_floors_is_a_configuration_error() {
    let env = [
        ("GM_H2_ADDR", ":0"),
        ("GM_TLS_CERT", "/missing/cert.pem"),
        ("GM_TLS_KEY", "/missing/key.pem"),
        ("GM_MAX_CONNECTIONS", "2"),
        ("GM_MAX_CONNECTIONS_PER_CLIENT", "2"),
        ("GM_MAX_BUFFER_BYTES", "3407871"),
    ];
    let output = server(&[], &env, b"");
    assert_eq!(output.status.code(), Some(1));
    let line = text(&output.stderr).strip_suffix('\n').unwrap();
    let message = "configuration error: \"GM_MAX_BUFFER_BYTES (3407871) must be at least 3407872: GM_MAX_CONNECTIONS \
                   (2) connection floors of 1572864 bytes, 0 bytes of QUIC endpoint buffers and the 262144-byte \
                   download block\"";
    assert_eq!(logged(line), message);
}

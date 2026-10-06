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

/// A serving server with `env` and its log's messages.
#[cfg(unix)]
fn serving(env: &[(&str, &str)]) -> (std::process::Child, impl Iterator<Item = String> + Send + use<>) {
    use std::io::{BufRead, BufReader};
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
        .env_clear()
        .envs(env.iter().copied())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let lines = BufReader::new(child.stderr.take().unwrap()).lines();
    (child, lines.map(|line| logged(&line.unwrap()).2.to_owned()))
}

/// Stops `child` with SIGTERM and expects exit 0.
#[cfg(unix)]
fn terminate(mut child: std::process::Child) {
    let killed = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
    assert!(killed.unwrap().success());
    assert!(child.wait().unwrap().success());
}

/// The level, topic and message of a plain log line after its `YYYY-MM-DDTHH:MM:SSZ ` time.
fn logged(line: &str) -> (&str, &str, &str) {
    let (stamp, rest) = line.split_at(21);
    let shape = stamp
        .bytes()
        .map(|byte| if byte.is_ascii_digit() { b'0' } else { byte })
        .collect::<Vec<_>>();
    assert_eq!(shape, b"0000-00-00T00:00:00Z ", "{line}");
    (rest[..5].trim_end(), rest[6..15].trim_end(), &rest[16..])
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
fn configuration_errors_log_an_error_line_and_exit_one() {
    let output = server(&[], &[("GM_PUBLIC_ORIGINS", "bad\"origin")], b"");
    assert_eq!(output.status.code(), Some(1));
    let line = text(&output.stderr).strip_suffix('\n').unwrap();
    let message = r#"GM_PUBLIC_ORIGINS contains invalid origin "bad\"origin""#;
    assert_eq!(logged(line), ("ERROR", "config", message));
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

/// A verbose throughput line's message, checked for its shape, with its transfer count.
fn transfers(message: &str) -> usize {
    let fields: Vec<_> = message.split(" · ").collect();
    let decimal = |field: &str| {
        let (whole, fraction) = field.split_once('.').unwrap();
        assert!(whole.bytes().all(|byte| byte.is_ascii_digit()) && fraction.len() == 2, "{message}");
    };
    let [rate, count, bytes] = fields[..] else { panic!("{message}") };
    decimal(rate.strip_suffix(" Gbit/s").unwrap());
    decimal(bytes.split_once(" MB in ").unwrap().0);
    count.strip_suffix(" transfers").unwrap().parse().unwrap()
}

#[cfg(unix)]
#[test]
fn a_verbose_server_reports_throughput_and_sigterm_ends_its_running_download() {
    use std::io::Read;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let address = format!("127.0.0.1:{port}");
    let (child, lines) = serving(&[("GM_H1_ADDR", &address), ("GM_VERBOSE", "true"), ("TOKIO_WORKER_THREADS", "2")]);
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || lines.map(|line| sender.send(line)).take_while(Result::is_ok).count());
    let next = || received.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    let role = "HTTP/1.1 clear (UI, discovery, probe, transfers, WebSockets)";
    assert_eq!(next(), format!("graphite-meter {ENGINE_VERSION} starting"));
    assert_eq!(next(), format!("listening on {address}/tcp: {role}"));
    let mut download = std::net::TcpStream::connect(&address).unwrap();
    download
        .write_all(b"GET /download?bytes=68719476736 HTTP/1.1\r\nHost: test\r\n\r\n")
        .unwrap();
    download.read_exact(&mut [0; 1 << 16]).unwrap();
    assert_eq!(transfers(&next()), 1, "the running download");
    let stopping = std::time::Instant::now();
    let reading = std::thread::spawn(move || download.read_to_end(&mut Vec::new()).is_ok());
    terminate(child);
    assert!(reading.join().unwrap(), "the download ends with the server");
    let elapsed = stopping.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(2), "stopped after {elapsed:?}");
}

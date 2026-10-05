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

/// Password authentication behind a trusted proxy on a clear listener, with the hash setting `hash`.
fn password_auth(hash: (&'static str, &'static str)) -> [(&'static str, &'static str); 6] {
    [
        ("GM_H1_ADDR", "127.0.0.1:0"),
        ("GM_AUTH_MODE", "password"),
        ("GM_AUTH_PUBLIC_URL", "https://meter.example"),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "none"),
        ("GM_PUBLIC_ORIGINS", "https://meter.example"),
        hash,
    ]
}

#[cfg(unix)]
#[test]
fn a_bad_password_hash_refuses_startup_and_a_good_one_logs_the_mode() {
    use std::io::{BufRead, BufReader};
    for (hash, message) in [
        (
            ("GM_AUTH_PASSWORD_HASH", "$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$a2V5"),
            "password hash must use m=19456,t=2,p=1",
        ),
        (
            ("GM_AUTH_PASSWORD_HASH_FILE", "/nonexistent/hash"),
            "password hash: open /nonexistent/hash: no such file or directory",
        ),
    ] {
        let output = server(&[], &password_auth(hash), b"");
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(logged(text(&output.stderr).trim_end()), format!("server error: \"{message}\""));
    }
    let hash = "$argon2id$v=19$m=19456,t=2,p=1$OT2po7nOdP+21BKX5CuZQw$9kVgfSWvlFy31939zUCVY62fHIuSqC8RwL67EpQ8qy8";
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
        .env_clear()
        .envs(password_auth(("GM_AUTH_PASSWORD_HASH", hash)))
        .env("GM_VERBOSE", "true")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let mut next = || lines.next().unwrap().unwrap();
    assert_eq!(logged(&next()), "[gm:auth:debug] local password hash loaded and validated");
    let mode = "mode=password origin=https://meter.example provider=Authelia issuer= allowed-groups=0";
    assert_eq!(logged(&next()), format!("[gm:auth] {mode} session-lifetime=8h0m0s"));
    let killed = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
    assert!(killed.unwrap().success());
    assert!(child.wait().unwrap().success());
}

/// A verbose throughput line's message, checked against Go's shape, with its transfer count.
fn transfers(message: &str) -> usize {
    let fields = message.strip_prefix("[gm:server:download] ").unwrap();
    let fields: Vec<_> = fields.split(" · ").collect();
    let decimal = |field: &str| {
        let (whole, fraction) = field.split_once('.').unwrap();
        assert!(whole.bytes().all(|byte| byte.is_ascii_digit()) && fraction.len() == 2, "{message}");
    };
    let [rate, count, bytes] = fields[..] else { panic!("{message}") };
    decimal(rate.strip_suffix(" Gbit/s").unwrap());
    decimal(bytes.strip_suffix(" MB this window").unwrap());
    count.strip_suffix(" conns").unwrap().parse().unwrap()
}

#[cfg(unix)]
#[test]
fn verbose_logs_report_throughput_and_sigterm_ends_a_running_download() {
    use std::io::{BufRead, BufReader, Read};
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let address = format!("127.0.0.1:{port}");
    let env = [("GM_H1_ADDR", address.as_str()), ("GM_VERBOSE", "true"), ("TOKIO_WORKER_THREADS", "2")];
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
        .env_clear()
        .envs(env)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (lines, received) = std::sync::mpsc::channel();
    let stderr = BufReader::new(child.stderr.take().unwrap());
    std::thread::spawn(move || {
        stderr
            .lines()
            .map_while(Result::ok)
            .try_for_each(|line| lines.send(line))
    });
    let next = || logged(&received.recv_timeout(std::time::Duration::from_secs(5)).unwrap()).to_owned();
    assert!(next().contains(" listening on "));
    let mut download = std::net::TcpStream::connect(&address).unwrap();
    download
        .write_all(b"GET /download?bytes=68719476736 HTTP/1.1\r\nHost: test\r\n\r\n")
        .unwrap();
    download.read_exact(&mut [0; 1 << 16]).unwrap();
    assert_eq!(transfers(&next()), 1, "the running download");
    let stopping = std::time::Instant::now();
    let killed = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
    assert!(killed.unwrap().success());
    let mut rest = Vec::new();
    download.read_to_end(&mut rest).unwrap();
    assert!(child.wait().unwrap().success());
    let elapsed = stopping.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(2), "stopped after {elapsed:?}");
}

/// What a server with HTTP/3 and four connections holds on a current-thread runtime, so on one endpoint: every
/// connection's floor with the download block, and the endpoint's buffers.
fn single_endpoint_terms(env: &[(&str, &str)]) -> (usize, usize) {
    use graphite_meter_server::{config, runtime::Server};
    let lookup = |name: &str| env.iter().find(|(key, _)| *key == name).map(|(_, value)| value.into());
    let config = |budget: &str| {
        let lookup = |name: &str| match name {
            "GM_MAX_BUFFER_BYTES" => Some(budget.into()),
            _ => lookup(name),
        };
        let loaded = config::load(lookup, Vec::<std::ffi::OsString>::new(), &mut Vec::new());
        let Ok(config::Loaded::Config(config)) = loaded else { panic!("{loaded:?}") };
        *config
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let server = Server::bind(config("8589934592")).await.unwrap();
        let endpoint = server.budget().usage().reserved;
        let refusal = Server::bind(config("1")).await.err().unwrap();
        let term = |suffix: &str| -> usize {
            let before = refusal.split(suffix).next().unwrap();
            before.rsplit(' ').next().unwrap().parse().unwrap()
        };
        (term(": GM_MAX_CONNECTIONS") - term(" bytes of QUIC endpoint buffers"), endpoint)
    })
}

#[cfg(target_os = "linux")]
#[test]
fn a_budget_covering_fewer_quic_endpoints_than_planned_logs_how_many() {
    use graphite_meter_testkit::{Identity, Scratch};
    use std::io::{BufRead, BufReader};
    let (scratch, identity) = (Scratch::new().unwrap(), Identity::generate().unwrap());
    let cert = scratch.file("cert.pem", &identity.certificate).unwrap();
    let key = scratch.file("key.pem", &identity.key).unwrap();
    let mut env = vec![
        ("GM_H1_ADDR", "127.0.0.1:0"),
        ("GM_H3_ADDR", "127.0.0.7:0"),
        ("GM_TLS_CERT", cert.to_str().unwrap()),
        ("GM_TLS_KEY", key.to_str().unwrap()),
        ("GM_MAX_CONNECTIONS", "4"),
        ("GM_MAX_CONNECTIONS_PER_CLIENT", "4"),
    ];
    // Two endpoints hold more than one, so a budget covering exactly one falls back to it.
    let (rest, endpoint) = single_endpoint_terms(&env);
    let budget = (rest + endpoint).to_string();
    env.extend([("GM_MAX_BUFFER_BYTES", budget.as_str()), ("TOKIO_WORKER_THREADS", "4")]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
        .env_clear()
        .envs(env)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap())
        .lines()
        .map_while(Result::ok);
    let messages: Vec<_> = lines
        .by_ref()
        .map(|line| logged(&line).to_owned())
        .take_while(|message| !message.contains("/udp ("))
        .collect();
    let line = "[gm:memory] the buffer budget covers 1 of 2 QUIC endpoints";
    assert!(messages.iter().any(|message| message == line), "{messages:?}");
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

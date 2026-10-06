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
    (child, lines.map(|line| logged(&line.unwrap()).to_owned()))
}

/// Stops `child` with SIGTERM and expects exit 0.
#[cfg(unix)]
fn terminate(mut child: std::process::Child) {
    let killed = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
    assert!(killed.unwrap().success());
    assert!(child.wait().unwrap().success());
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
    let mut env = password_auth(("GM_AUTH_PASSWORD_HASH", hash)).to_vec();
    env.push(("GM_VERBOSE", "true"));
    let (child, mut lines) = serving(&env);
    assert_eq!(lines.next().unwrap(), "[gm:auth:debug] local password hash loaded and validated");
    let mode = "mode=password origin=https://meter.example provider=Authelia issuer= allowed-groups=0";
    assert_eq!(lines.next().unwrap(), format!("[gm:auth] {mode} session-lifetime=8h0m0s"));
    terminate(child);
}

#[test]
fn oidc_mode_refuses_startup_without_its_client_secret_or_until_discovery_succeeds() {
    let oidc = |secret| {
        [
            ("GM_H1_ADDR", "127.0.0.1:0"),
            ("GM_AUTH_MODE", "oidc"),
            ("GM_AUTH_PUBLIC_URL", "https://meter.example"),
            ("GM_ADVERTISED_NATIVE_ENDPOINTS", "none"),
            ("GM_PUBLIC_ORIGINS", "https://meter.example"),
            ("GM_AUTH_OIDC_ISSUER", "https://127.0.0.1:1"),
            ("GM_AUTH_OIDC_CLIENT_ID", "meter"),
            ("GM_AUTH_OIDC_ALLOWED_GROUPS", "operators"),
            ("GM_VERBOSE", "true"),
            secret,
        ]
    };
    let output = server(&[], &oidc(("GM_AUTH_OIDC_CLIENT_SECRET_FILE", "/nonexistent/secret")), b"");
    assert_eq!(output.status.code(), Some(1));
    let message = "server error: \"OIDC client secret: open /nonexistent/secret: no such file or directory\"";
    assert_eq!(logged(text(&output.stderr).trim_end()), message);
    let output = server(&[], &oidc(("GM_AUTH_OIDC_CLIENT_SECRET", "s3cret")), b"");
    assert_eq!(output.status.code(), Some(1));
    let lines: Vec<_> = text(&output.stderr).lines().map(logged).collect();
    assert!(lines[0].starts_with("[gm:auth] mode=oidc origin=https://meter.example provider=Authelia "));
    assert!(lines[1].starts_with("[gm:auth:debug] OIDC discovery failed: "), "{lines:?}");
    assert!(lines[2].starts_with("server error: \"OIDC discovery: "), "{lines:?}");
    assert!(!text(&output.stderr).contains("s3cret"));
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
    let role = "HTTP/1.1 clear: UI, discovery, probe, transfers, WebSockets";
    assert_eq!(next(), format!("graphite-meter {ENGINE_VERSION} listening on {address}/tcp ({role})"));
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

/// The `[gm:tls]` lines a server logs before listening with a certificate for `days` days and a key with `mode`.
#[cfg(unix)]
fn tls_lines(days: &str, mode: u32) -> (Vec<String>, String, String) {
    use std::os::unix::fs::PermissionsExt;
    let scratch = graphite_meter_testkit::Scratch::new().unwrap();
    let (cert, key) = (scratch.path().join("cert.pem"), scratch.path().join("key.pem"));
    let (cert, key) = (cert.to_str().unwrap(), key.to_str().unwrap());
    let curve = ["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-subj", "/CN=localhost"];
    let generated = Command::new("openssl")
        .args(["req", "-x509", "-days", days, "-keyout", key, "-out", cert])
        .args(curve)
        .args(["-addext", "subjectAltName=DNS:localhost"])
        .output()
        .unwrap();
    assert!(generated.status.success(), "{generated:?}");
    std::fs::set_permissions(key, std::fs::Permissions::from_mode(mode)).unwrap();
    let end = Command::new("openssl")
        .args(["x509", "-in", cert, "-noout", "-enddate", "-dateopt", "iso_8601"])
        .output()
        .unwrap();
    let end = text(&end.stdout)
        .trim()
        .strip_prefix("notAfter=")
        .unwrap()
        .replace(' ', "T");
    let (child, lines) = serving(&[("GM_H1_TLS_ADDR", "127.0.0.1:0"), ("GM_TLS_CERT", cert), ("GM_TLS_KEY", key)]);
    let tls = lines.take_while(|line| !line.contains(" listening on ")).collect();
    terminate(child);
    (tls, end, key.to_owned())
}

#[cfg(unix)]
#[test]
fn a_loaded_certificate_logs_its_expiry_and_warns_of_a_near_one_and_a_readable_key() {
    let (lines, end, key) = tls_lines("10", 0o644);
    assert_eq!(
        lines,
        [
            format!("[gm:tls] certificate loaded; expires at {end}"),
            "[gm:tls] warning: certificate expires in 240h0m0s".into(),
            format!("[gm:tls] warning: private key {key} permissions are 0644; remove group/other access"),
        ]
    );
    let (lines, end, _) = tls_lines("90", 0o600);
    assert_eq!(lines, [format!("[gm:tls] certificate loaded; expires at {end}")]);
}

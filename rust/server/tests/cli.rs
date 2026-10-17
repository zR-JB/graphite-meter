use graphite_meter_server::{assets::NOTICES, config::ENGINE_VERSION};
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

/// The level, topic and message of a plain log line after its RFC 3339 UTC time.
fn logged(line: &str) -> (&str, &str, &str) {
    let (stamp, rest) = line.split_once(' ').unwrap();
    let shape: String = stamp
        .chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect();
    assert_eq!(shape, "0000-00-00T00:00:00Z", "{line}");
    (rest[..5].trim_end(), rest[6..16].trim_end(), &rest[17..])
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
fn hash_password_prints_the_hash_of_a_confirmed_password() {
    use argon2::{Algorithm, Argon2, Params, Version};
    use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
    let output = server(&["hash-password"], &[], b"correct horse\r\ncorrect horse\n");
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), "Password: Confirm password: ");
    let hash = text(&output.stdout).strip_suffix('\n').unwrap();
    let ["", "argon2id", "v=19", "m=19456,t=2,p=1", salt, key] = hash.split('$').collect::<Vec<_>>()[..] else {
        panic!("{hash}")
    };
    let (salt, key) = (STANDARD_NO_PAD.decode(salt).unwrap(), STANDARD_NO_PAD.decode(key).unwrap());
    let mut derived = [0; 32];
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::new(19456, 2, 1, Some(32)).unwrap());
    argon2
        .hash_password_into(b"correct horse", &salt, &mut derived)
        .unwrap();
    assert_eq!(derived[..], key[..]);

    for (stdin, prompts, message) in [
        (&b"one\ntwo\n"[..], "Password: Confirm password: ", "passwords do not match"),
        (b"\n", "Password: ", "password must contain 1 to 1024 bytes"),
        (&[b'x'; 1100], "Password: ", "password must contain 1 to 1024 bytes"),
    ] {
        let output = server(&["hash-password"], &[], stdin);
        assert_eq!((output.status.code(), text(&output.stdout)), (Some(1), ""), "{message}");
        let line = text(&output.stderr)
            .strip_prefix(prompts)
            .unwrap()
            .strip_suffix('\n')
            .unwrap();
        assert_eq!(logged(line), ("ERROR", "auth:", message));
    }
}

#[test]
fn legal_prints_the_embedded_notices_or_refuses_without_them() {
    let output = server(&["--legal"], &[], b"");
    let Some(report) = NOTICES.report() else {
        assert_eq!(output.status.code(), Some(1));
        let line = text(&output.stderr).strip_suffix('\n').unwrap();
        let message = "this build embeds no notices; mise run dev -- --legal builds the server with dependency notices \
                       and prints them";
        return assert_eq!(logged(line), ("ERROR", "legal:", message));
    };
    assert!(output.status.success());
    assert_eq!(output.stdout, report);
}

#[test]
fn configuration_errors_log_an_error_line_and_exit_one() {
    let output = server(&[], &[("GM_PUBLIC_ORIGINS", "bad\"origin")], b"");
    assert_eq!(output.status.code(), Some(1));
    let line = text(&output.stderr).strip_suffix('\n').unwrap();
    let message = r#"GM_PUBLIC_ORIGINS contains invalid origin "bad\"origin""#;
    assert_eq!(logged(line), ("ERROR", "config:", message));
}

#[test]
fn help_prints_the_usage_and_exits_zero() {
    let output = server(&["-h"], &[], b"");
    assert!(output.status.success());
    assert_eq!(text(&output.stderr), include_str!("usage.txt"));
    let output = server(&["-nope"], &[], b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("unknown option --nope\nUsage: "));
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
    let place = format!("{address}/tcp");
    assert_eq!(next(), format!("graphite-meter {ENGINE_VERSION} starting"));
    assert!(next().starts_with("serving as "));
    assert_eq!(next(), format!("{place:<21} HTTP/1.1  UI, discovery, probe, transfers, WebSockets"));
    assert_eq!(next(), "ready");
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

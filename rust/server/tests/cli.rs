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

/// A server serving HTTP/1.1 on a local port with `env`: the child, its address and its next log message.
#[cfg(unix)]
fn listening(env: &[(&str, &str)]) -> (std::process::Child, String, impl Fn() -> String) {
    use std::io::{BufRead, BufReader};
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
        .env_clear()
        .envs([("GM_H1_ADDR", "127.0.0.1:0"), ("TOKIO_WORKER_THREADS", "2")])
        .envs(env.iter().copied())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        lines
            .map(|line| sender.send(logged(&line.unwrap()).2.to_owned()))
            .take_while(Result::is_ok)
            .count()
    });
    let next = move || {
        received
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
    };
    assert_eq!(next(), format!("graphite-meter {ENGINE_VERSION} starting"));
    assert!(next().starts_with("serving as "));
    let listen = next();
    let (place, roles) = listen.split_once(' ').unwrap();
    assert_eq!(
        roles.trim_start(),
        "HTTP/1.1  UI, discovery, probe, transfers, WebSockets"
    );
    assert_eq!(next(), "ready");
    (child, place.strip_suffix("/tcp").unwrap().to_owned(), next)
}

#[cfg(unix)]
fn terminate(mut child: std::process::Child) {
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    assert!(killed.unwrap().success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the server outlived SIGTERM by 10 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(status.success());
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
    let ["", "argon2id", "v=19", "m=19456,t=2,p=1", salt, key] =
        hash.split('$').collect::<Vec<_>>()[..]
    else {
        panic!("{hash}")
    };
    let (salt, key) = (
        STANDARD_NO_PAD.decode(salt).unwrap(),
        STANDARD_NO_PAD.decode(key).unwrap(),
    );
    let mut derived = [0; 32];
    let argon2 = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(19456, 2, 1, Some(32)).unwrap(),
    );
    argon2
        .hash_password_into(b"correct horse", &salt, &mut derived)
        .unwrap();
    assert_eq!(derived[..], key[..]);

    for (stdin, prompts, message) in [
        (
            &b"one\ntwo\n"[..],
            "Password: Confirm password: ",
            "passwords do not match",
        ),
        (b"\n", "Password: ", "password must contain 1 to 1024 bytes"),
        (
            &[b'x'; 1100],
            "Password: ",
            "password must contain 1 to 1024 bytes",
        ),
    ] {
        let output = server(&["hash-password"], &[], stdin);
        assert_eq!(
            (output.status.code(), text(&output.stdout)),
            (Some(1), ""),
            "{message}"
        );
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
fn a_listener_that_cannot_bind_names_its_setting() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = taken.local_addr().unwrap().to_string();
    let env = [
        ("GM_H1_ADDR", address.as_str()),
        ("TOKIO_WORKER_THREADS", "2"),
    ];
    let output = server(&[], &env, b"");
    assert_eq!(output.status.code(), Some(1));
    let failed = text(&output.stderr).lines().last().map(logged).unwrap();
    let message = format!("could not listen on {address}/tcp (GM_H1_ADDR): ");
    assert_eq!(failed.0, "ERROR");
    assert!(failed.2.starts_with(&message), "{failed:?}");
}

#[test]
fn help_prints_the_usage_and_exits_zero() {
    let output = server(&["-h"], &[], b"");
    assert!(output.status.success());
    assert_eq!(text(&output.stdout), include_str!("usage.txt"));
    assert!(output.stderr.is_empty());
    let output = server(&["-nope"], &[], b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("unknown option --nope\nUsage: "));
    let output = server(&["help"], &[], b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).starts_with("unexpected argument \"help\"\nUsage: "));
}

/// A verbose throughput line's message, checked for its shape, with its transfer count.
fn transfers(message: &str) -> usize {
    let fields: Vec<_> = message.split(" · ").collect();
    let decimal = |field: &str| {
        let (whole, fraction) = field.split_once('.').unwrap();
        assert!(
            whole.bytes().all(|byte| byte.is_ascii_digit()) && fraction.len() == 2,
            "{message}"
        );
    };
    let [rate, count, bytes] = fields[..] else {
        panic!("{message}")
    };
    decimal(rate.strip_suffix(" Gbit/s").unwrap());
    decimal(bytes.split_once(" MB in ").unwrap().0);
    count.strip_suffix(" transfers").unwrap().parse().unwrap()
}

#[cfg(unix)]
#[test]
fn healthcheck_passes_while_the_clear_listener_answers() {
    let (child, address, _) = listening(&[]);
    let check = || server(&["healthcheck"], &[("GM_H1_ADDR", address.as_str())], b"");
    let healthy = check();
    assert!(healthy.status.success(), "{}", text(&healthy.stderr));
    assert!(healthy.stdout.is_empty() && healthy.stderr.is_empty());
    terminate(child);
    let down = check();
    assert_eq!(down.status.code(), Some(1));
    let line = text(&down.stderr).strip_suffix('\n').unwrap();
    let message = format!("could not ask {address} for /probe: connection refused");
    assert_eq!(logged(line), ("ERROR", "health:", message.as_str()));
}

#[test]
fn healthcheck_fails_a_listener_that_never_answers() {
    let hung = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = hung.local_addr().unwrap().to_string();
    let output = server(&["healthcheck"], &[("GM_H1_ADDR", address.as_str())], b"");
    assert_eq!(output.status.code(), Some(1));
    let line = text(&output.stderr).strip_suffix('\n').unwrap();
    let message = format!("{address} did not answer /probe within 2s");
    assert_eq!(logged(line), ("ERROR", "health:", message.as_str()));
}

#[cfg(unix)]
#[test]
fn a_private_key_open_beyond_group_read_is_warned_about() {
    use graphite_meter_testkit::{Identity, Scratch};
    use std::io::{BufRead, BufReader};
    let (scratch, identity) = (Scratch::new().unwrap(), Identity::generate().unwrap());
    let cert = scratch.file("cert.pem", &identity.certificate).unwrap();
    for (mode, warned) in [(0o600, false), (0o640, false), (0o660, true), (0o644, true)] {
        let key = scratch
            .file_with_mode("key.pem", &identity.key, mode)
            .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
            .env_clear()
            .envs([
                ("GM_H1_ADDR", "127.0.0.1:0"),
                ("GM_H2_ADDR", "127.0.0.2:0"),
                ("GM_TLS_CERT", cert.to_str().unwrap()),
                ("GM_TLS_KEY", key.to_str().unwrap()),
                ("TOKIO_WORKER_THREADS", "2"),
            ])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let started: Vec<_> = lines
            .map(Result::unwrap)
            .take_while(|line| logged(line).2 != "ready")
            .collect();
        let warning = format!("private key {} has permissions {mode:04o}", key.display());
        let found = started
            .iter()
            .any(|line| logged(line).2.starts_with(&warning));
        terminate(child);
        assert_eq!(found, warned, "{mode:04o}: {started:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_verbose_server_logs_requests_and_throughput_and_sigterm_ends_its_running_download() {
    use std::io::Read;
    let (child, address, next) = listening(&[("GM_VERBOSE", "true")]);
    let mut download = std::net::TcpStream::connect(address).unwrap();
    download
        .write_all(b"GET /download?bytes=68719476736 HTTP/1.1\r\nHost: test\r\n\r\n")
        .unwrap();
    download.read_exact(&mut [0; 1 << 16]).unwrap();
    // The browser suite's network group reads these lines.
    assert_eq!(next(), "GET /download from 127.0.0.1 on H1: 200");
    assert_eq!(transfers(&next()), 1, "the running download");
    let stopping = std::time::Instant::now();
    let reading = std::thread::spawn(move || download.read_to_end(&mut Vec::new()).is_ok());
    terminate(child);
    assert!(reading.join().unwrap(), "the download ends with the server");
    let elapsed = stopping.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "stopped after {elapsed:?}"
    );
}

#[cfg(unix)]
fn ask_retired(address: &str, times: usize) {
    use std::io::Read;
    for _ in 0..times {
        let mut old = std::net::TcpStream::connect(address).unwrap();
        old.write_all(b"POST /upload/session HTTP/1.1\r\nHost: test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        old.read_to_end(&mut Vec::new()).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn a_route_an_earlier_release_served_is_logged_once_a_minute_per_client() {
    let (child, address, next) = listening(&[]);
    ask_retired(&address, 2);
    assert_eq!(
        next(),
        format!(
            "POST /upload/session from 127.0.0.1: a route Graphite Meter 0.13.5 removed, 1 since its last line; \
             update that client to {ENGINE_VERSION} (app::gate)"
        )
    );
    terminate(child);
    assert!(
        next().starts_with("stop requested"),
        "the second request waits for the next minute"
    );
}

#[cfg(unix)]
#[test]
fn a_retired_route_is_logged_before_sign_in_refuses_it() {
    use graphite_meter_testkit::Scratch;
    use std::io::{BufRead, BufReader};
    let hash = server(&["hash-password"], &[], b"secret\nsecret\n");
    let scratch = Scratch::new().unwrap();
    let file = scratch.file("hash", text(&hash.stdout)).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
        .env_clear()
        .envs([
            ("GM_H1_ADDR", "127.0.0.1:0"),
            ("TOKIO_WORKER_THREADS", "2"),
            ("GM_AUTH_MODE", "password"),
            ("GM_AUTH_PUBLIC_URL", "https://meter.example.com"),
            ("GM_AUTH_PASSWORD_HASH_FILE", file.to_str().unwrap()),
            ("GM_ADVERTISED_NATIVE_ENDPOINTS", "none"),
            ("GM_PUBLIC_ORIGINS", "self"),
        ])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap())
        .lines()
        .map(Result::unwrap);
    let listen = lines
        .by_ref()
        .map(|line| logged(&line).2.to_owned())
        .find(|message| message.contains("/tcp"))
        .unwrap();
    let address = listen.split_once("/tcp").unwrap().0.to_owned();
    assert!(lines.by_ref().any(|line| logged(&line).2 == "ready"));
    ask_retired(&address, 1);
    let line = lines.next().unwrap();
    assert!(
        logged(&line)
            .2
            .starts_with("POST /upload/session from 127.0.0.1: a route"),
        "{line}"
    );
    terminate(child);
}

#[cfg(unix)]
#[test]
fn connections_past_a_client_share_are_logged_once_a_minute_with_a_count() {
    use std::io::Read;
    let (child, address, next) = listening(&[("GM_MAX_CONNECTIONS_PER_CLIENT", "1")]);
    let mut held = std::net::TcpStream::connect(&address).unwrap();
    held.write_all(b"GET /probe HTTP/1.1\r\nHost: test\r\n\r\n")
        .unwrap();
    held.read_exact(&mut [0; 12]).unwrap();
    for _ in 0..3 {
        let mut refused = std::net::TcpStream::connect(&address).unwrap();
        assert!(
            matches!(refused.read(&mut [0; 1]), Ok(0) | Err(_)),
            "closed unserved"
        );
    }
    assert_eq!(
        next(),
        "connections from 127.0.0.1 refused: GM_MAX_CONNECTIONS_PER_CLIENT=1 reached, 1 since its last line; set \
         GM_TRUSTED_PROXIES for a proxy, or raise the limit for clients sharing an address (limits)"
    );
    drop(held);
    terminate(child);
    assert!(
        next().starts_with("stop requested"),
        "the later refusals wait for the next minute"
    );
}

use futures_util::{SinkExt, StreamExt};
use graphite_meter_client::Error;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
};

async fn read_header(stream: &mut tokio::net::TcpStream) -> Result<(), Error> {
    let mut header = Vec::with_capacity(512);
    while header.len() < 4096 {
        let mut byte = [0];
        stream.read_exact(&mut byte).await?;
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            return Ok(());
        }
    }
    Err("oversized request header".into())
}

/// The text without its SGR colour sequences.
fn unpainted(text: &str) -> String {
    let mut parts = text.split("\x1b[");
    let mut plain = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        match part.find(|c: char| !c.is_ascii_digit() && c != ';') {
            Some(end) if part[end..].starts_with('m') => plain.push_str(&part[end + 1..]),
            _ => {
                plain.push_str("\x1b[");
                plain.push_str(part);
            }
        }
    }
    plain
}

fn client(origin: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_graphite-meter-client"));
    command.args([
        "--report",
        "-url",
        origin,
        "-stages",
        "latency",
        "-latency-duration",
        "1s",
        "-warmup",
        "0",
        "-ping",
        "80ms",
    ]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    command
}

async fn latency_peer(revoked_download: bool) -> Result<(String, tokio::task::JoinHandle<()>), Error> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let twin = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let twin_origin = format!("http://{}", twin.local_addr()?);
    let peer = tokio::spawn(async move {
        while let Ok((mut stream, _)) =
            tokio::select! { accepted = listener.accept() => accepted, accepted = twin.accept() => accepted }
        {
            let twin_origin = twin_origin.clone();
            tokio::spawn(async move {
                let mut request = [0; 4096];
                let path = loop {
                    let size = stream.peek(&mut request).await?;
                    if size == 0 {
                        return Ok::<_, Error>(());
                    }
                    if let Some(end) = request[..size].windows(2).position(|pair| pair == b"\r\n") {
                        break std::str::from_utf8(&request[..end])?
                            .split_whitespace()
                            .nth(1)
                            .ok_or("missing path")?
                            .to_owned();
                    }
                    tokio::task::yield_now().await;
                };
                if path == "/ws/ping" {
                    let mut socket = tokio_tungstenite::accept_async(stream).await?;
                    while let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) = socket.next().await {
                        let id = graphite_meter_core::wire::decode_ping(&text)?;
                        socket
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                graphite_meter_core::wire::encode_pong(id, 0).into(),
                            ))
                            .await?;
                    }
                    return Ok(());
                }
                read_header(&mut stream).await?;
                if path.starts_with("/download") {
                    stream.write_all(b"HTTP/1.1 403 authentication required\r\nX-Graphite-Upload-Refusal: revoked\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
                    return Ok(());
                }
                let throughput = if revoked_download {
                    serde_json::json!([{"baseUrl":".","transport":"fetch-stream","protocol":"http1"}])
                } else {
                    serde_json::json!([])
                };
                let body = match path.as_str() {
                    "/servers" if revoked_download => serde_json::json!({"defaultSelection":["self","twin"],"servers":[{"id":"self","url":".","name":"local peer"},{"id":"twin","url":twin_origin,"name":"twin peer"}]}),
                    "/servers" => serde_json::json!({"defaultSelection":["self"],"servers":[{"id":"self","url":".","name":"local peer"}]}),
                    "/preflight" => serde_json::json!({"generation":"cli-fixture","capabilities":{"throughput":throughput,"latency":[{"baseUrl":".","transport":"websocket"}]}}),
                    "/probe" => serde_json::json!({"clientIp":"127.0.0.1","clientIpVersion":4,"clientIpSource":"socket","protocolNegotiated":"http/1.1"}),
                    _ => return Err("unexpected route".into()),
                }.to_string();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await?;
                Ok(())
            });
        }
    });
    Ok((origin, peer))
}

#[tokio::test]
async fn report_runs_a_real_latency_path_and_exits_complete() -> Result<(), Error> {
    let (origin, peer) = latency_peer(false).await?;
    let output = tokio::time::timeout(Duration::from_secs(5), client(&origin).output()).await??;
    peer.abort();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout)?;
    assert!(
        report.starts_with("Graphite Meter  Complete  local peer · "),
        "{report}"
    );
    let idle = report
        .lines()
        .find(|line| line.starts_with("Idle "))
        .unwrap_or_default();
    assert!(
        idle.rsplit_once("  ")
            .is_some_and(|(_, timeouts)| timeouts.starts_with("0 / ")),
        "{report}"
    );
    assert!(
        report.contains("Server handling of the mean round trip: Idle "),
        "{report}"
    );
    assert_eq!(String::from_utf8(output.stderr)?, "Latency…\n");
    Ok(())
}

#[path = "../../test_identity.rs"]
mod test_identity;

/// As Go, the client runs without a trust store: cleartext paths never load one, and a TLS
/// connection it cannot verify fails alone, with the certificate's reason.
#[tokio::test]
async fn an_empty_trust_store_fails_only_tls_connections() -> Result<(), Error> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    let nowhere = "/nonexistent/graphite-meter-roots";
    let run = |origin: &str| {
        let mut command = client(origin);
        command.env("SSL_CERT_FILE", nowhere).env("SSL_CERT_DIR", nowhere);
        tokio::time::timeout(Duration::from_secs(5), command.output())
    };
    let (origin, peer) = latency_peer(false).await?;
    let cleartext = run(&origin).await??;
    peer.abort();
    assert_eq!(
        cleartext.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&cleartext.stderr)
    );
    let _ = graphite_meter_client::crypto::provider().install_default();
    let (certificate, key) = test_identity::generate_identity("localhost")?;
    let tls = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(
        vec![CertificateDer::from_pem_slice(certificate.as_bytes())?],
        PrivateKeyDer::from_pem_slice(key.as_bytes())?,
    )?;
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(tls));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("https://{}", listener.local_addr()?);
    let peer = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move { acceptor.accept(socket).await.map(drop) });
        }
    });
    let secure = run(&origin).await??;
    peer.abort();
    assert_eq!(secure.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(secure.stderr)?,
        "graphite-meter-client: Test could not start: Certificate not trusted: certificate signed by unknown \
         authority. Turn on Skip TLS verify (-insecure) only for a server you trust.\n"
    );
    Ok(())
}

#[tokio::test]
async fn sign_in_refused_after_measuring_ends_incomplete() -> Result<(), Error> {
    let (origin, peer) = latency_peer(true).await?;
    let output = tokio::time::timeout(
        Duration::from_secs(8),
        client(&origin)
            .args(["-stages", "latency,download", "-download-duration", "1s"])
            .output(),
    )
    .await??;
    peer.abort();
    let report = String::from_utf8(output.stdout)?;
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(
        report.starts_with("Graphite Meter  Incomplete  2 servers · "),
        "{report}"
    );
    assert!(report.contains("\ntwin peer · Download throughput · at 1."), "{report}");
    assert!(report.contains(" · Sign-in required\n"), "{report}");
    println!("{report}");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn report_signal_exit_codes_and_failure_keep_the_final_outcome() -> Result<(), Error> {
    for (signal, code) in [("-INT", 130), ("-TERM", 143)] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let child = client(&origin).spawn()?;
        let (_pending, _) = listener.accept().await?;
        assert!(
            Command::new("kill")
                .args([signal, &child.id().ok_or("child exited")?.to_string()])
                .status()
                .await?
                .success()
        );
        let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await??;
        assert_eq!(output.status.code(), Some(code));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8(output.stderr)?,
            "graphite-meter-client: Test stopped before it started.\n"
        );
    }
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        read_header(&mut socket).await?;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .await?;
        Ok::<_, Error>(())
    });
    let output = tokio::time::timeout(Duration::from_secs(5), client(&origin).output()).await??;
    peer.await??;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.starts_with("graphite-meter-client: Test could not start: "),
        "{stderr}"
    );
    let (origin, peer) = latency_peer(true).await?;
    peer.abort();
    let output = tokio::time::timeout(Duration::from_secs(5), client(&origin).output()).await??;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr)?,
        "graphite-meter-client: Test could not start: Server could not be reached\n"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn tui_quit_and_interrupt_exit_like_the_go_client() -> Result<(), Error> {
    let (origin, peer) = latency_peer(false).await?;
    let script = r#"
import fcntl, json, os, pty, select, struct, subprocess, sys, termios, time
master, slave = pty.openpty()
fcntl.fcntl(master, fcntl.F_SETFL, os.O_NONBLOCK)
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
def session():
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
p = subprocess.Popen([sys.argv[1], '-url', sys.argv[2], '-stages', 'latency', '-latency-duration', '5s', '-warmup', '0', '-ping', '80ms'], stdin=subprocess.DEVNULL if sys.argv[3] == 'redirected-quit' else slave, stdout=slave, stderr=slave, preexec_fn=session, env={**os.environ, 'TERM':'xterm-256color'})
os.close(slave)
output = b''
step = 0
mark = 0
mode = sys.argv[3]
measuring = '░'.encode()
keys = {'check-quit': [(b'Checking', b'q')],
        'redirected-quit': [(b'WebSocket', b'q')],
        'setup-interrupt': [(b'WebSocket', b'\x03')],
        'run-interrupt': [(b'WebSocket', b'r'), (measuring, b'\x03')],
        'run-quit': [(b'WebSocket', b'r'), (measuring, b'q')],
        'run-abort': [(b'WebSocket', b'r'), (measuring, b'\x03\x03')],
        'confirmed-stop': [(b'WebSocket', b'r'), (measuring, b'\x1b'), (b'confirm', b'\x1b'), (b'Stopped', b'q')]}[mode]
deadline = time.monotonic() + 8
answered = False
try:
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.2)[0]:
            try:
                data = os.read(master, 65536)
            except OSError:
                break
            output += data
            # Answer the device-attributes query like a terminal, so the background query ends at once.
            if not answered and b'\x1b[c' in output:
                os.write(master, b'\x1b[?62c')
                answered = True
            if step < len(keys) and keys[step][0] in output[mark:]:
                os.write(master, keys[step][1])
                step += 1
                mark = len(output)
        if p.poll() is not None:
            while select.select([master], [], [], 0)[0]:
                try:
                    output += os.read(master, 65536)
                except OSError:
                    break
            break
    try:
        code = p.wait(timeout=max(0, deadline - time.monotonic()))
    except subprocess.TimeoutExpired:
        raise RuntimeError(f'TUI stop did not finish at step {step}: {output[-1000:]!r}')
    print(json.dumps({'code':code, 'step':step, 'text':output.decode('utf-8', 'replace')}))
finally:
    if p.poll() is None:
        p.kill()
        p.wait()
    os.close(master)
"#;
    let silent = TcpListener::bind("127.0.0.1:0").await?;
    let silent_origin = format!("http://{}", silent.local_addr()?);
    for (mode, step, code, report) in [
        ("check-quit", 1, 0, false),
        // Like Go, the interface opens when only stdout is a terminal.
        ("redirected-quit", 1, 0, false),
        ("setup-interrupt", 1, 0, false),
        ("run-interrupt", 2, 130, true),
        ("run-quit", 2, 1, true),
        ("run-abort", 2, 130, false),
        ("confirmed-stop", 4, 1, true),
    ] {
        let output = Command::new("python3")
            .args([
                "-c",
                script,
                env!("CARGO_BIN_EXE_graphite-meter-client"),
                if mode == "check-quit" { &silent_origin } else { &origin },
                mode,
            ])
            .output()
            .await?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(result["step"], step);
        assert_eq!(result["code"], code);
        let text = result["text"].as_str().unwrap();
        assert!(text.contains("\x1b[?25h"));
        let (_, printed) = text
            .rsplit_once("\x1b[?1049l")
            .ok_or("the alternate screen stayed open")?;
        // The terminal gets the report in Go's colours.
        assert_eq!(
            unpainted(printed).contains("Graphite Meter  Stopped"),
            report,
            "{mode}: {printed:?}"
        );
        assert_eq!(unpainted(printed) != printed, report, "{mode}: {printed:?}");
    }
    peer.abort();
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn tui_asks_the_terminal_for_its_background_like_the_go_client() -> Result<(), Error> {
    let script = r#"
import fcntl, json, os, pty, select, struct, subprocess, sys, termios, time
master, slave = pty.openpty()
fcntl.fcntl(master, fcntl.F_SETFL, os.O_NONBLOCK)
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
def session():
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
env = {k: v for k, v in os.environ.items() if k not in ('NO_COLOR', 'GM_TUI_THEME', 'COLORFGBG')}
env.update(TERM='xterm-256color', COLORTERM='truecolor')
p = subprocess.Popen([sys.argv[1], '-url', sys.argv[2]], stdin=subprocess.DEVNULL if sys.argv[4] == 'redirected' else slave, stdout=slave, stderr=slave, preexec_fn=session, env=env)
os.close(slave)
answer = sys.argv[3].encode()
output, drawn, alive, started = b'', None, None, time.monotonic()
try:
    while time.monotonic() < started + 8 and p.poll() is None:
        if select.select([master], [], [], 0.1)[0]:
            try:
                output += os.read(master, 65536)
            except OSError:
                break
            if answer and b'\x1b[c' in output:
                os.write(master, answer)
                answer = b''
            if drawn is None and b'Graphite Meter' in output:
                drawn = time.monotonic() - started
                time.sleep(0.3)
                alive = p.poll() is None
                os.write(master, b'q')
    code = p.wait(timeout=5)
    print(json.dumps({'code':code, 'drawn':drawn, 'alive':alive, 'text':output.decode('utf-8', 'replace')}))
finally:
    if p.poll() is None:
        p.kill()
        p.wait()
    os.close(master)
"#;
    let silent = TcpListener::bind("127.0.0.1:0").await?;
    let silent_origin = format!("http://{}", silent.local_addr()?);
    // The q in the unknown OSC would quit the TUI if the answers reached its keys. With stdin
    // redirected, the answer arrives on the terminal device, where the keys are read.
    for (answer, ink, stdin) in [
        (
            "\x1b]99;q\x07\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62;22c",
            "48;2;32;36;42m",
            "terminal",
        ),
        ("", "48;2;230;232;234m", "terminal"),
        (
            "\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62;22c",
            "48;2;32;36;42m",
            "redirected",
        ),
    ] {
        let output = Command::new("python3")
            .args([
                "-c",
                script,
                env!("CARGO_BIN_EXE_graphite-meter-client"),
                &silent_origin,
                answer,
                stdin,
            ])
            .output()
            .await?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let text = result["text"].as_str().unwrap();
        assert!(text.contains("\x1b]11;?\x1b\\\x1b[c"), "{text:?}");
        assert!(text.contains(ink), "{answer:?} with {stdin} stdin: {text:?}");
        assert_eq!(result["code"], 0);
        assert_eq!(result["alive"], true);
        assert!(result["drawn"].as_f64().is_some_and(|drawn| drawn < 3.0), "{result}");
    }
    Ok(())
}

#[tokio::test]
async fn invalid_measurement_inputs_fail_before_connecting() -> Result<(), Error> {
    for args in [
        vec!["--throughput-transport=webtransport-datagram"],
        vec!["--stages=typo"],
        vec!["--ping=typo"],
        vec!["--loaded-ping=40ms"],
        vec!["--warmup=-1s"],
        vec!["--download-duration=0"],
        vec!["--server=a", "--server=a"],
        vec!["--latency-transport=webtransport", "--ping=16s"],
        vec!["--throughput-origin=https://user:secret@example.com"],
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(2),
            Command::new(env!("CARGO_BIN_EXE_graphite-meter-client"))
                .args(&args)
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
    Ok(())
}

async fn flags(args: &[&str]) -> Result<std::process::Output, Error> {
    Ok(tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(env!("CARGO_BIN_EXE_graphite-meter-client"))
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await??)
}

#[tokio::test]
async fn invalid_settings_are_refused_in_go_order_and_words() -> Result<(), Error> {
    for (args, message) in [
        (
            &["-stages", "", "-warmup", "5s"][..],
            "select at least one stage: latency, download, upload or bidirectional",
        ),
        (
            &["-warmup", "5s", "-latency-duration", "0"],
            "warmup must be from 0 s to 4 s",
        ),
        (
            &["-latency-duration", "0", "-ping", "40ms"],
            "latency duration must be from 1 s to 300 s",
        ),
        // Go checks the duration of a stage that is off.
        (
            &["-bidirectional-duration", "301s"],
            "bidirectional duration must be from 1 s to 300 s",
        ),
        (
            &["-loaded-ping", "40ms", "-streams", "15"],
            "latency cadence must be reply-driven or at least 80ms",
        ),
        // Zero is a fixed cadence in Go, not reply-driven.
        (&["-ping", "0"], "latency cadence must be reply-driven or at least 80ms"),
        (
            &["--loaded-ping=0s"],
            "latency cadence must be reply-driven or at least 80ms",
        ),
        (
            &["-streams", "15", "-auto-streams", "0"],
            "forced streams must be from 1 to 14 per server and direction, or 0 for automatic",
        ),
        (
            &["-auto-streams", "0", "-ping", "16s"],
            "the automatic stream maximum must be from 1 to 14 per direction",
        ),
        (
            &["-ping", "16s"],
            "latency interval must be at most 15s, half the server's 30s lane idle bound",
        ),
    ] {
        let output = flags(args).await?;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(
            String::from_utf8(output.stderr)?,
            format!("graphite-meter-client: {message}\n"),
            "{args:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn version_names_the_client_like_go() -> Result<(), Error> {
    for flag in ["-version", "--version"] {
        let output = flags(&[flag]).await?;
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        let version = String::from_utf8(output.stdout)?;
        assert!(
            version
                .strip_prefix("graphite-meter-client ")
                .is_some_and(|number| number.contains("-rust") && !number.trim_end().contains(' ')),
            "{version:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn help_goes_to_stderr_like_go() -> Result<(), Error> {
    for flag in ["-h", "-help", "--help", "--h", "-h=1"] {
        let output = flags(&[flag]).await?;
        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert!(output.stdout.is_empty(), "{flag}");
        assert_eq!(
            String::from_utf8(output.stderr)?,
            graphite_meter_client::cli::HELP,
            "{flag}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn flag_errors_print_go_messages_then_the_usage() -> Result<(), Error> {
    for (args, message) in [
        (&["-x", "-url"][..], "flag provided but not defined: -x"),
        (&["-version", "--x=1"], "flag provided but not defined: -x"),
        (&["-url"], "flag needs an argument: -url"),
        (&["---x"], "bad flag syntax: ---x"),
        (
            &["-report=maybe"],
            r#"invalid boolean value "maybe" for -report: parse error"#,
        ),
        (
            &["-legal=maybe"],
            r#"invalid boolean value "maybe" for -legal: parse error"#,
        ),
        (&["-warmup", "x"], r#"invalid value "x" for flag -warmup: parse error"#),
        (
            &["-streams", "99999999999999999999"],
            r#"invalid value "99999999999999999999" for flag -streams: value out of range"#,
        ),
        (
            &["-stages", "typo"],
            r#"invalid value "typo" for flag -stages: unknown stage "typo": use latency, download, upload, or bidirectional"#,
        ),
        (
            &["-ping", "x"],
            r#"invalid value "x" for flag -ping: use reply-driven, fast, medium, slow, or a duration such as 400ms"#,
        ),
        (
            &["-server", "a", "-server", "a"],
            r#"invalid value "a" for flag -server: select one to 4 different server IDs"#,
        ),
    ] {
        let output = flags(args).await?;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(
            String::from_utf8(output.stderr)?,
            format!("{message}\n{}", graphite_meter_client::cli::HELP),
            "{args:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn arguments_and_checks_after_parsing_fail_like_go() -> Result<(), Error> {
    for (args, message) in [
        (&["foo", "-x"][..], r#"unexpected argument "foo""#),
        (&["--", "-x"], r#"unexpected argument "-x""#),
        (&["-"], r#"unexpected argument "-""#),
        (&["-report", "maybe"], r#"unexpected argument "maybe""#),
        (
            &["-throughput-protocol", "spdy", "-throughput-protocol", "http4"],
            r#"invalid throughput protocol "http4": use auto, http1, http2, or http3"#,
        ),
        (
            &["-throughput-transport", "webtransport-datagram"],
            r#"invalid throughput transport "webtransport-datagram": use auto, fetch-stream, or webtransport"#,
        ),
        (
            &["-latency-transport", "x"],
            r#"invalid latency transport "x": use auto, websocket, or webtransport"#,
        ),
        (&["-warmup", "-1s"], "warmup must be from 0 s to 4 s"),
        (
            &["-upload-duration", "-1s"],
            "upload duration must be from 1 s to 300 s",
        ),
        (
            &["-streams", "-1"],
            "forced streams must be from 1 to 14 per server and direction, or 0 for automatic",
        ),
        (
            &["-ping", "-1s"],
            "latency cadence must be reply-driven or at least 80ms",
        ),
    ] {
        let output = flags(args).await?;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(
            String::from_utf8(output.stderr)?,
            format!("graphite-meter-client: {message}\n"),
            "{args:?}"
        );
    }
    Ok(())
}

/// -version and -legal act once every flag parses, before arguments and settings are checked.
#[tokio::test]
async fn version_and_legal_act_after_parsing() -> Result<(), Error> {
    let version = flags(&["-version", "foo"]).await?;
    assert_eq!(version.status.code(), Some(0));
    assert!(version.stdout.starts_with(b"graphite-meter-client "));
    let legal = flags(&["--legal"]).await?;
    for args in [&["-legal"][..], &["--legal=true", "foo"], &["-url", "--legal"]] {
        let output = flags(args).await?;
        assert_eq!(output.status.code(), legal.status.code(), "{args:?}");
        assert_eq!(
            (&output.stdout, &output.stderr),
            (&legal.stdout, &legal.stderr),
            "{args:?}"
        );
    }
    let output = flags(&["-legal=false", "-version"]).await?;
    assert_eq!(output.stdout, version.stdout);
    Ok(())
}

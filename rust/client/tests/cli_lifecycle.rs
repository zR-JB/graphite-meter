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
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn latency_peer() -> Result<(String, tokio::task::JoinHandle<()>), Error> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let peer = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
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
                    while let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) =
                        socket.next().await
                    {
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
                let body = match path.as_str() {
                    "/servers" => serde_json::json!({"defaultSelection":["self"],"servers":[{"id":"self","url":".","name":"local peer"}]}),
                    "/preflight" => serde_json::json!({"generation":"cli-fixture","capabilities":{"throughput":[],"latency":[{"baseUrl":".","transport":"websocket"}]}}),
                    "/probe" => serde_json::json!({"clientIp":"127.0.0.1","clientIpVersion":4,"clientIpSource":"socket","protocolNegotiated":"http/1.1"}),
                    _ => return Err("unexpected route".into()),
                }.to_string();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
                Ok(())
            });
        }
    });
    Ok((origin, peer))
}

#[tokio::test]
async fn report_runs_a_real_latency_path_and_exits_complete() -> Result<(), Error> {
    let (origin, peer) = latency_peer().await?;
    let output = tokio::time::timeout(Duration::from_secs(5), client(&origin).output()).await??;
    peer.abort();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout)?;
    assert!(report.contains("Complete"));
    assert!(report.contains("local peer: Median"));
    assert!(report.contains("Probe timeouts 0/") && report.contains("(0.0%)"));
    assert!(report.contains("replies · 1.0 s"));
    assert!(report.contains("Server timing (") && report.contains("paired replies, means): raw"));
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
        let output =
            tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await??;
        assert_eq!(output.status.code(), Some(code));
        assert!(String::from_utf8(output.stdout)?.contains("Stopped"));
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
    assert!(String::from_utf8(output.stdout)?.contains("Failed"));
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn tui_stop_and_raw_interrupt_preserve_exit_reason() -> Result<(), Error> {
    let (origin, peer) = latency_peer().await?;
    let script = r#"
import fcntl, json, os, pty, select, struct, subprocess, sys, termios, time
master, slave = pty.openpty()
fcntl.fcntl(master, fcntl.F_SETFL, os.O_NONBLOCK)
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
def session():
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
p = subprocess.Popen([sys.argv[1], '-url', sys.argv[2], '-stages', 'latency', '-latency-duration', '5s', '-warmup', '0', '-ping', '80ms'], stdin=slave, stdout=slave, stderr=slave, preexec_fn=session, env={**os.environ, 'TERM':'xterm-256color'})
os.close(slave)
output = b''
step = 0
mode = sys.argv[3]
deadline = time.monotonic() + 8
try:
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.2)[0]:
            try:
                data = os.read(master, 65536)
            except OSError:
                break
            output += data
            if step == 0 and b'WebSocket' in output:
                os.write(master, b'\x03' if mode == 'setup-interrupt' else b'r')
                step = 1
            elif step == 1 and b'Running' in output:
                os.write(master, b'\x03' if mode == 'run-interrupt' else b'\x1b')
                step = 2
            elif step == 2 and b'confirm stop' in output:
                os.write(master, b'\x1b')
                step = 3
            elif step == 3 and b'Stopped' in output:
                os.write(master, b'q')
                step = 4
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
    for (mode, step, code) in [
        ("setup-interrupt", 1, 130),
        ("run-interrupt", 2, 130),
        ("confirmed-stop", 4, 1),
    ] {
        let output = Command::new("python3")
            .args([
                "-c",
                script,
                env!("CARGO_BIN_EXE_graphite-meter-client"),
                &origin,
                mode,
            ])
            .output()
            .await?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(result["step"], step);
        assert_eq!(result["code"], code);
        let text = result["text"].as_str().unwrap();
        assert!(text.contains("\x1b[?1049l"));
        assert!(text.contains("\x1b[?25h"));
        if mode != "setup-interrupt" {
            assert!(
                result["text"]
                    .as_str()
                    .unwrap()
                    .contains("Graphite Meter · Stopped"),
                "{mode}: {text:?}"
            );
        }
    }
    peer.abort();
    Ok(())
}

#[tokio::test]
async fn invalid_measurement_inputs_fail_before_connecting() -> Result<(), Error> {
    for args in [
        vec!["--throughput-transport=webtransport-datagram"],
        vec!["--stages=typo"],
        vec!["--ping=typo"],
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
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
    Ok(())
}

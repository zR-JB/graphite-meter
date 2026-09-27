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

#[tokio::test]
async fn report_runs_a_real_latency_path_and_exits_complete() -> Result<(), Error> {
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
    assert!(report.contains("Probe timeouts 0%"));
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

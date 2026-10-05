//! The binary without the interface against canned peers: signals during preparation, the second signal's policy, a
//! protected server, and a run whose server refuses its download.
#![cfg(unix)]

use graphite_meter_client::{INTERRUPTED, Interrupts, Reaction, TERMINATED};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::{Child, Command},
    time::timeout,
};

/// A run of `stages` for a second each in report mode at `url`, without proxies.
fn client(url: &str, stages: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_graphite-meter-client"));
    command.args(["--report", "-url", url, "-stages", stages]);
    command.args(["-latency-duration", "1s", "-download-duration", "1s", "-loaded-latency=false"]);
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY"] {
        command.env_remove(name).env_remove(name.to_ascii_lowercase());
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

/// The client at a peer that accepts its catalogue request and never answers, once the request arrived.
async fn preparing() -> (Child, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let child = client(&format!("http://{}", listener.local_addr().unwrap()), "latency")
        .spawn()
        .unwrap();
    let (mut held, _) = listener.accept().await.unwrap();
    let _ = held.read(&mut [0; 1024]).await;
    tokio::spawn(async move { held.read(&mut [0; 1]).await });
    (child, listener)
}

async fn kill(child: &Child, signals: &[&str]) {
    let pid = child.id().unwrap().to_string();
    for signal in signals {
        assert!(
            Command::new("kill")
                .args([*signal, pid.as_str()])
                .status()
                .await
                .unwrap()
                .success()
        );
    }
}

async fn finished(child: Child) -> (Option<i32>, String, String) {
    let output = timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let text = |bytes| String::from_utf8(bytes).unwrap();
    (output.status.code(), text(output.stdout), text(output.stderr))
}

#[tokio::test]
async fn a_termination_during_preparation_stops_the_test_before_it_starts() {
    let (child, _peer) = preparing().await;
    kill(&child, &["-TERM"]).await;
    let stopped = "graphite-meter-client: Test stopped before it started.\n";
    assert_eq!(finished(child).await, (Some(143), String::new(), stopped.into()));
}

#[test]
fn the_first_signal_stops_and_a_second_exits_with_its_own_status_in_either_order() {
    for (first, second) in [(INTERRUPTED, TERMINATED), (TERMINATED, INTERRUPTED)] {
        let mut interrupts = Interrupts::default();
        assert_eq!(interrupts.on(first), Reaction::Stop(first));
        assert_eq!(interrupts.on(second), Reaction::Exit(second));
        assert_eq!(interrupts.on(first), Reaction::Exit(first));
    }
}

#[tokio::test]
async fn a_protected_server_exits_1_asking_for_a_terminal() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let _peer = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") && stream.read_u8().await.map(|byte| head.push(byte)).is_ok() {}
            let refusal = "HTTP/1.1 403 Forbidden\r\ngraphite-meter-auth: required\r\ncontent-length: 0\r\n\r\n";
            let _ = stream.write_all(refusal.as_bytes()).await;
        }
    });
    let refused = "graphite-meter-client: Sign-in required; run graphite-meter-client in a terminal to sign in.\n";
    let child = client(&url, "latency").spawn().unwrap();
    assert_eq!(finished(child).await, (Some(1), String::new(), refused.into()));
}

/// A server named A at a peer that answers its catalogue, preflight and probe, and refuses everything else.
async fn refusing_downloads() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                loop {
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") {
                        let Ok(byte) = stream.read_u8().await else { return };
                        head.push(byte);
                    }
                    let path = String::from_utf8_lossy(&head)
                        .split(' ')
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let body = match path.as_str() {
                        "/servers" => r#"{"defaultSelection":["self"],"servers":[{"id":"self","url":".","name":"A"}]}"#,
                        "/preflight" => concat!(
                            r#"{"server":{"name":"A","location":""},"engineVersion":"1","generation":"1","#,
                            r#""capabilities":{"uploadCheckpoint":true,"maxStageMs":300000,"#,
                            r#""throughput":[{"baseUrl":".","transport":"fetch-stream","protocol":"http1"}],"#,
                            r#""latency":[{"baseUrl":".","transport":"websocket"}]}}"#
                        ),
                        "/probe" => {
                            r#"{"clientIp":"127.0.0.1","clientIpVersion":4,"clientIpSource":"socket","protocolNegotiated":"http/1.1"}"#
                        }
                        _ => "",
                    };
                    let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
                    let answer = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    if stream.write_all(answer.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    url
}

#[tokio::test]
async fn a_run_whose_server_left_ends_its_report_with_why() {
    let child = client(&refusing_downloads().await, "download").spawn().unwrap();
    let (status, report, stderr) = finished(child).await;
    assert_eq!((status, stderr.as_str()), (Some(1), ""), "{report}");
    assert!(report.starts_with("Graphite Meter  Failed  A · "), "{report}");
    assert!(
        report.ends_with(" s\n\nall selected servers failed: HTTP 404 from /download\n"),
        "{report}"
    );
    assert_eq!(report.lines().count(), 3, "{report}");
}

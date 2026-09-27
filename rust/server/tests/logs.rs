mod support;

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

fn timestamped(line: &str) -> Option<&str> {
    let (stamp, message) = line.split_at_checked(20)?;
    let digits = stamp.bytes().enumerate().all(|(index, byte)| match index {
        4 | 7 => byte == b'/',
        10 | 19 => byte == b' ',
        13 | 16 => byte == b':',
        _ => byte.is_ascii_digit(),
    });
    digits.then_some(message)
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn log_lines_match_go_and_peer_failures_are_limited() {
    let identity = support::Identity::generate();
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
            .env_clear()
            .envs([
                ("GM_H1_ADDR", "127.0.0.1:0"),
                ("GM_H1_TLS_ADDR", "127.0.0.2:0"),
                ("GM_H1_PUBLIC_ORIGIN", "http://localhost:7246"),
                ("GM_H1_TLS_PUBLIC_ORIGIN", "https://localhost:7247"),
                (
                    "GM_TLS_CERT",
                    identity.directory().join("identity.pem").to_str().unwrap(),
                ),
                (
                    "GM_TLS_KEY",
                    identity.directory().join("identity.key").to_str().unwrap(),
                ),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let (lines, received) = mpsc::channel();
    let stderr = BufReader::new(server.0.stderr.take().unwrap());
    let reader = std::thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            let _ = lines.send(line);
        }
    });
    let listening = |role: &str| loop {
        let line = received
            .recv_timeout(Duration::from_secs(10))
            .expect("timestamped listener line");
        if let Some(address) = timestamped(&line).and_then(|message| message.strip_suffix(&format!("/tcp ({role})"))) {
            return address.rsplit_once(" listening on ").unwrap().1.to_owned();
        }
    };
    let clear = listening("HTTP/1.1 clear: UI, discovery, probe, transfers, WebSockets");
    let tls = listening("HTTPS/WSS HTTP/1.1: UI, discovery, probe, transfers, WebSockets");
    for _ in 0..2 {
        let mut plain = TcpStream::connect(&tls).unwrap();
        plain.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let _ = plain.read_to_end(&mut Vec::new());
    }
    for host in ["a.example", "b.example"] {
        let mut request = TcpStream::connect(&clear).unwrap();
        write!(
            request,
            "GET /servers HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        request.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    }
    drop(server);
    reader.join().unwrap();
    let rest: Vec<_> = received.try_iter().collect();
    let failures: Vec<_> = rest.iter().filter(|line| line.contains("[gm:http]")).collect();
    assert_eq!(failures.len(), 1, "{rest:?}");
    let failure = timestamped(failures[0]).unwrap();
    assert!(
        failure.starts_with("[gm:http] http: TLS handshake error from 127.0.0."),
        "{failure}"
    );
    assert!(!rest.iter().any(|line| line.contains("[gm:discovery]")), "{rest:?}");
}

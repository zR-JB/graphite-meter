mod support;

use std::{
    io::{BufRead, BufReader},
    net::{SocketAddr, UdpSocket},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::JoinHandle,
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

/// The server serving `identity` with only `env` besides, and its stderr lines.
fn start(identity: &support::Identity, env: [(&str, &str); 2]) -> (Server, mpsc::Receiver<String>, JoinHandle<()>) {
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
            .env_clear()
            .env("GM_TLS_CERT", identity.directory().join("identity.pem"))
            .env("GM_TLS_KEY", identity.directory().join("identity.key"))
            .envs(env)
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
    (server, received, reader)
}

/// A close reason that would add a whole forged log line and clear the operator's terminal.
const FORGED: &[u8] = b"\n2026/01/01 00:00:00 [gm:auth] forged line\x1b[2J";

/// A client's first Initial holding nothing but a CONNECTION_CLOSE with `reason`: anyone can send one.
fn closing_initial(reason: &[u8]) -> Vec<u8> {
    let destination = [0x5a; 8];
    let keys = rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256
        .tls13()
        .and_then(|suite| suite.quic_suite())
        .unwrap()
        .keys(&destination, rustls::Side::Client, rustls::quic::Version::V1);
    // QUIC v1 Initial: the destination ID, no source ID or token, a two-byte length and a one-byte packet number.
    let mut header = vec![0xc0, 0, 0, 0, 1, destination.len() as u8];
    header.extend_from_slice(&destination);
    header.extend_from_slice(&[0, 0, 0, 0, 0]);
    let number = header.len() - 1;
    // CONNECTION_CLOSE with PROTOCOL_VIOLATION and no frame type; PADDING fills the datagram to 1200 bytes.
    let mut payload = vec![0x1c, 0x0a, 0x00, u8::try_from(reason.len()).unwrap()];
    payload.extend_from_slice(reason);
    let tag = keys.local.packet.tag_len();
    payload.resize(1200 - header.len() - tag, 0);
    let length = u16::try_from(1 + payload.len() + tag).unwrap() | 0x4000;
    header[number - 2..number].copy_from_slice(&length.to_be_bytes());
    let tag = keys.local.packet.encrypt_in_place(0, &header, &mut payload).unwrap();
    payload.extend_from_slice(tag.as_ref());
    // The protection sample starts four bytes past the packet number's start.
    let (first, rest) = header.split_first_mut().unwrap();
    keys.local
        .header
        .encrypt_in_place(&payload[3..3 + 16], first, &mut rest[number - 1..])
        .unwrap();
    header.extend_from_slice(&payload);
    header
}

#[test]
fn a_peers_handshake_close_reason_cannot_forge_log_lines() {
    let identity = support::Identity::generate();
    let (server, received, reader) = start(&identity, [("GM_H1_ADDR", "127.0.0.2:0"), ("GM_H3_ADDR", "127.0.0.1:0")]);
    let next = || received.recv_timeout(Duration::from_secs(10)).expect("a log line");
    let quic: SocketAddr = loop {
        let line = next();
        if let Some(address) = timestamped(&line)
            .and_then(|message| message.strip_suffix("/udp (HTTP/3: probe, transfers, progress, WebTransport)"))
        {
            break address.rsplit_once(" listening on ").unwrap().1.parse().unwrap();
        }
    };
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.send_to(&closing_initial(FORGED), quic).unwrap();
    let failure = loop {
        let line = next();
        if line.contains("[gm:h3]") {
            break line;
        }
    };
    drop(server);
    reader.join().unwrap();
    let rest: Vec<_> = received.try_iter().collect();
    let failure = timestamped(&failure).unwrap();
    let error = "[gm:h3] QUIC handshake error from 127.0.0.1: ";
    assert!(failure.starts_with(error), "{failure}");
    // The peer's words stay in its one line, quoted, with their controls escaped.
    let quoted = r"\n2026/01/01 00:00:00 [gm:auth] forged line";
    assert!(failure.contains(quoted), "{failure}");
    assert!(!failure.contains('\u{1b}'), "{failure:?}");
    assert!(!rest.iter().any(|line| line.contains("forged")), "{rest:?}");
}

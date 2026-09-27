//! An owned WebSocket session measures raw RTT on Tokio's monotonic clock.
//! Native CLI grants authenticate the handshake directly: they cannot mint browser tickets.
use crate::{Error, net::Http};
use futures_util::{SinkExt, StreamExt};
use graphite_meter_core::{
    discovery::{LatencyTarget, LatencyTransport},
    latency::ProbeOutcome,
    origin::canonical_origin,
    wire,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
use tokio_tungstenite::tungstenite::{
    Message, client::IntoClientRequest, protocol::WebSocketConfig,
};

type Socket = tokio_tungstenite::WebSocketStream<Box<dyn graphite_meter_net::Stream>>;

#[derive(Clone, Copy, Debug)]
pub enum Observation {
    ConnectionBoundary,
    Sample {
        sent: Instant,
        received: Instant,
        rtt: Duration,
        server_handling: Duration,
    },
    Lost {
        sent: Instant,
        outcome: ProbeOutcome,
    },
}
impl Observation {
    pub fn outcome(self) -> Option<ProbeOutcome> {
        Some(match self {
            Self::ConnectionBoundary => return None,
            Self::Sample {
                rtt,
                server_handling,
                ..
            } => ProbeOutcome::Reply {
                // run() bounds duration to the core's signed nanosecond clock range.
                rtt_nanos: rtt.as_nanos() as i64,
                handling_nanos: server_handling.as_nanos() as u64,
            },
            Self::Lost { outcome, .. } => outcome,
        })
    }
}

/// The caller must validate this selected origin against its catalogue/preflight.
/// Backpressure is a measurement error, never silently dropped observations.
pub async fn run(
    http: &Http,
    origin: &str,
    insecure: bool,
    interval: Duration,
    duration: Duration,
    observations: mpsc::Sender<Observation>,
    cancel: watch::Receiver<bool>,
) -> Result<(), Error> {
    run_kind(
        http,
        origin,
        insecure,
        (interval, duration, if interval.is_zero() { 4 } else { 16 }),
        observations,
        cancel,
        Kind::WebSocket,
    )
    .await
}

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    WebSocket,
    WebTransport,
}

pub(crate) async fn run_kind(
    http: &Http,
    origin: &str,
    insecure: bool,
    timing: (Duration, Duration, usize),
    observations: mpsc::Sender<Observation>,
    mut cancel: watch::Receiver<bool>,
    kind: Kind,
) -> Result<(), Error> {
    let (interval, duration, window) = timing;
    if duration.is_zero() || duration.as_nanos() > i64::MAX as u128 {
        return Err("latency interval and bounded duration must be positive".into());
    }
    let Some(mut socket) = connect(http, origin, insecure, &mut cancel, kind).await? else {
        return Ok(());
    };
    let end = Instant::now()
        .checked_add(duration)
        .ok_or("latency duration exceeds clock range")?;
    loop {
        let result = measure(socket, interval, window, end, &observations, &mut cancel).await;
        let Err(error) = result else {
            return Ok(());
        };
        if !error.is::<Disconnected>() {
            return Err(error);
        }
        emit(&observations, Observation::ConnectionBoundary)?;
        let reconnect_until = (Instant::now() + Duration::from_secs(2)).min(end);
        socket = loop {
            if Instant::now() >= end {
                return Ok(());
            }
            let attempt = tokio::time::timeout_at(
                reconnect_until,
                connect(http, origin, insecure, &mut cancel, kind),
            )
            .await;
            match attempt {
                Ok(Ok(Some(socket))) => break socket,
                Ok(Ok(None)) => return Ok(()),
                Ok(Err(error)) if error.is::<crate::net::AuthRequired>() => return Err(error),
                _ if Instant::now() >= end => return Ok(()),
                _ if Instant::now() >= reconnect_until => {
                    return Err("latency channel did not reconnect within two seconds".into());
                }
                _ => {}
            }
            tokio::select! {
                biased;
                () = cancelled(&mut cancel) => return Ok(()),
                () = tokio::time::sleep_until(reconnect_until) => {
                    if reconnect_until == end { return Ok(()); }
                    return Err("latency channel did not reconnect within two seconds".into());
                },
                () = tokio::time::sleep(Duration::from_millis(100)) => {},
            }
        };
    }
}

enum Bus {
    WebSocket(Box<Socket>),
    WebTransport(Arc<crate::webtransport::Session>),
}
enum Writer {
    WebSocket(futures_util::stream::SplitSink<Socket, Message>),
    WebTransport(Arc<crate::webtransport::Session>),
}
enum Reader {
    WebSocket(futures_util::stream::SplitStream<Socket>),
    WebTransport(Arc<crate::webtransport::Session>),
}
impl Bus {
    fn split(self) -> (Writer, Reader) {
        match self {
            Self::WebSocket(socket) => {
                let (writer, reader) = (*socket).split();
                (Writer::WebSocket(writer), Reader::WebSocket(reader))
            }
            Self::WebTransport(session) => (
                Writer::WebTransport(session.clone()),
                Reader::WebTransport(session),
            ),
        }
    }
}
impl Writer {
    async fn send(&mut self, text: String) -> Result<(), Error> {
        match self {
            Self::WebSocket(writer) => writer.send(Message::Text(text.into())).await?,
            Self::WebTransport(session) => session.send_datagram(text.as_bytes()).await?,
        }
        Ok(())
    }
    async fn close(self, reader: Reader) {
        if let (Self::WebSocket(writer), Reader::WebSocket(reader)) = (self, reader)
            && let Ok(mut socket) = writer.reunite(reader)
        {
            let _ = tokio::time::timeout(Duration::from_millis(250), socket.close(None)).await;
        }
    }
}
impl Reader {
    async fn next(&mut self) -> Option<Result<Message, Error>> {
        match self {
            Self::WebSocket(reader) => reader.next().await.map(|result| result.map_err(Into::into)),
            Self::WebTransport(session) => Some(session.recv_datagram().await.map(|bytes| {
                match String::from_utf8(bytes.to_vec()) {
                    Ok(text) => Message::Text(text.into()),
                    Err(_) => Message::Binary(bytes),
                }
            })),
        }
    }
}

/// Check the actual latency channel before a run starts. A successful HTTP
/// probe does not establish that QUIC datagrams or WebSocket pings work.
pub(crate) async fn verify(
    http: &Http,
    target: &LatencyTarget,
    insecure: bool,
) -> Result<(), Error> {
    let kind = match target.transport {
        LatencyTransport::WebSocket => Kind::WebSocket,
        LatencyTransport::WebTransport => Kind::WebTransport,
    };
    let attempt = async {
        let (_stop, mut cancel) = watch::channel(false);
        let bus = connect(http, &target.base_url, insecure, &mut cancel, kind)
            .await?
            .ok_or("latency verification cancelled")?;
        let (mut writer, mut reader) = bus.split();
        let result = async {
            let reply_window = match kind {
                Kind::WebTransport => Duration::from_millis(750),
                Kind::WebSocket => Duration::from_secs(3),
            };
            loop {
                writer.send(wire::encode_ping(0)).await?;
                let reply = tokio::time::timeout(reply_window, async {
                    loop {
                        match reader.next().await {
                            Some(Ok(Message::Text(text))) => {
                                if wire::decode_pong(&text).is_ok_and(|pong| pong.id == 0) {
                                    return Ok(());
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => {
                                return Err("latency channel closed before replying".into());
                            }
                            Some(Err(error)) => return Err(error),
                            _ => {}
                        }
                    }
                })
                .await;
                match reply {
                    Ok(result) => return result,
                    Err(_) if matches!(kind, Kind::WebTransport) => continue,
                    Err(error) => return Err(error.into()),
                }
            }
        }
        .await;
        writer.close(reader).await;
        result
    };
    tokio::time::timeout(Duration::from_secs(3), attempt)
        .await
        .map_err(|_| -> Error { "latency channel did not reply within three seconds".into() })?
}
async fn connect(
    http: &Http,
    origin: &str,
    insecure: bool,
    cancel: &mut watch::Receiver<bool>,
    kind: Kind,
) -> Result<Option<Bus>, Error> {
    match kind {
        Kind::WebSocket => Ok(connect_ws(http, origin, insecure, cancel)
            .await?
            .map(|socket| Bus::WebSocket(Box::new(socket)))),
        Kind::WebTransport => {
            let target = format!("{}/wt/ping", canonical_origin(origin)?);
            tokio::select! {biased;
                () = cancelled(cancel) => Ok(None),
                session = crate::webtransport::Session::dial(http, &target, insecure, Duration::from_secs(10)) => Ok(Some(Bus::WebTransport(Arc::new(session?)))),
            }
        }
    }
}

async fn connect_ws(
    http: &Http,
    origin: &str,
    insecure: bool,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Option<Socket>, Error> {
    let origin = canonical_origin(origin)?;
    let target = format!("{origin}/ws/ping");
    let websocket = if let Some(rest) = target.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        format!(
            "ws://{}",
            target
                .strip_prefix("http://")
                .ok_or("invalid WebSocket origin")?
        )
    };
    let mut request = websocket.into_client_request()?;
    if let Some(authorization) = http.authorization(&target) {
        if insecure {
            return Err("authenticated operation refuses insecure TLS".into());
        }
        request
            .headers_mut()
            .insert(http::header::AUTHORIZATION, authorization);
    }
    let mut tls = crate::tls::config(insecure)?;
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let tls = origin
        .starts_with("https://")
        .then(|| tokio_rustls::TlsConnector::from(Arc::new(tls)));
    let config = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(4096)
        .max_message_size(Some(1024))
        .max_frame_size(Some(1024));
    let connection = async {
        let connection = http.dial(&origin, tls.as_ref()).await?;
        let key = tokio_tungstenite::tungstenite::handshake::derive_accept_key(
            request.headers()["sec-websocket-key"].as_bytes(),
        );
        *request.uri_mut() = if connection.absolute_form {
            target.parse()?
        } else {
            "/ws/ping".parse()?
        };
        if let Some(authorization) = connection.proxy_authorization {
            request
                .headers_mut()
                .insert(http::header::PROXY_AUTHORIZATION, authorization);
        }
        let (mut sender, driver) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(connection.stream))
                .await?;
        tokio::spawn(driver.with_upgrades());
        let response = sender
            .send_request(request.map(|()| crate::net::empty()))
            .await?;
        if response.status() != http::StatusCode::SWITCHING_PROTOCOLS {
            http.check_status(&target, response.status(), response.headers())?;
            return Err("WebSocket upgrade was not accepted".into());
        }
        let headers = response.headers();
        let upgrade = headers
            .get(http::header::UPGRADE)
            .and_then(|value| value.to_str().ok());
        let connection = headers
            .get(http::header::CONNECTION)
            .and_then(|value| value.to_str().ok());
        if !upgrade.is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
            || !connection.is_some_and(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
            || !headers
                .get("sec-websocket-accept")
                .is_some_and(|value| value == key.as_str())
            || headers.contains_key("sec-websocket-protocol")
            || headers.contains_key("sec-websocket-extensions")
        {
            return Err("invalid WebSocket upgrade response".into());
        }
        let upgraded = hyper::upgrade::on(response).await?;
        let stream: Box<dyn graphite_meter_net::Stream> =
            Box::new(hyper_util::rt::TokioIo::new(upgraded));
        Ok::<_, Error>(
            tokio_tungstenite::WebSocketStream::from_raw_socket(
                stream,
                tokio_tungstenite::tungstenite::protocol::Role::Client,
                Some(config),
            )
            .await,
        )
    };
    let socket = tokio::select! {
        biased;
        () = cancelled(cancel) => return Ok(None),
        result = tokio::time::timeout(Duration::from_secs(10), connection) => result??,
    };
    Ok(Some(socket))
}

#[derive(Debug)]
struct Disconnected(&'static str);
impl std::fmt::Display for Disconnected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for Disconnected {}

async fn measure(
    socket: Bus,
    interval: Duration,
    window: usize,
    end: Instant,
    observations: &mpsc::Sender<Observation>,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), Error> {
    let (mut writer, mut reader) = socket.split();
    let mut pending = BTreeMap::<u32, (Instant, Instant)>::new();
    let mut next_id = 0_u32;
    let mut next_send = Instant::now();
    let mut sending = true;
    let mut srtt: Option<f64> = None;
    let mut variation = 0.0_f64;
    let mut expiry = tokio::time::interval(Duration::from_millis(50));
    let result = loop {
        if (!sending || Instant::now() >= end) && pending.is_empty() {
            break Ok(());
        }
        tokio::select! {
            biased;
            () = cancelled(cancel), if sending => {
                sending = false;
            }
            _ = expiry.tick() => {
                let now = Instant::now();
                let mut failure = None;
                pending.retain(|_, (sent, deadline)| {
                    if now < *deadline { return true; }
                    if let Err(error) = emit(observations, Observation::Lost { sent: *sent, outcome: ProbeOutcome::Timeout }) {
                        failure = Some(error);
                    }
                    false
                });
                if let Some(error) = failure { break Err(error); }
            }
            () = tokio::time::sleep_until(next_send), if sending && next_send < end => {
                let sent = Instant::now();
                let timeout = Duration::from_secs_f64(srtt.map_or(0.25, |mean| mean + 4.0 * variation.max(0.001)).clamp(0.25, 10.0));
                next_send = sent + if interval.is_zero() { timeout } else { interval };
                if pending.len() >= window { continue; }
                let id = next_id;
                let Some(next) = next_id.checked_add(1) else { break Err("latency probe identifier exhausted".into()); };
                next_id = next;
                pending.insert(id, (sent, sent + timeout));
                if !matches!(tokio::time::timeout(Duration::from_secs(1), writer.send(wire::encode_ping(id))).await, Ok(Ok(()))) {
                    pending.remove(&id);
                    if let Err(error) = emit(observations, Observation::Lost { sent, outcome: ProbeOutcome::SendFailure }) { break Err(error); }
                    break Err(Disconnected("latency channel send failed").into());
                }
            }
            message = reader.next() => {
                let received = Instant::now();
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(pong) = wire::decode_pong(&text) else { continue };
                        let Some((sent, deadline)) = pending.remove(&pong.id) else { continue };
                        let rtt = received.saturating_duration_since(sent);
                        let observation = if received >= deadline {
                            Observation::Lost { sent, outcome: ProbeOutcome::Timeout }
                        } else {
                            let seconds = rtt.as_secs_f64();
                            match srtt {
                                Some(mean) => { variation = 0.75 * variation + 0.25 * (mean - seconds).abs(); srtt = Some(0.875 * mean + 0.125 * seconds); }
                                None => { srtt = Some(seconds); variation = seconds / 2.0; }
                            }
                            if interval.is_zero() { next_send = received; }
                            Observation::Sample { sent, received, rtt, server_handling: Duration::from_nanos(pong.handling_nanos) }
                        };
                        if let Err(error) = emit(observations, observation) { break Err(error); }
                    }
                    Some(Ok(Message::Close(_))) | None => break Err(Disconnected("latency channel closed before measurement ended").into()),
                    Some(Err(_)) => break Err(Disconnected("latency channel receive failed").into()),
                    _ => {}
                }
            }
        }
    };
    for (_, (sent, _)) in pending {
        emit(
            observations,
            Observation::Lost {
                sent,
                outcome: ProbeOutcome::Unresolved,
            },
        )?;
    }
    writer.close(reader).await;
    result
}

fn emit(observations: &mpsc::Sender<Observation>, observation: Observation) -> Result<(), Error> {
    observations
        .try_send(observation)
        .map_err(|_| "latency observation consumer closed or fell behind".into())
}
async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    while !*cancel.borrow_and_update() {
        if cancel.changed().await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn stage_boundary_drains_replies_until_each_fixed_deadline() -> Result<(), Error> {
        for delay in [150, 300] {
            let (client, server) = tokio::io::duplex(4096);
            let socket = Socket::from_raw_socket(
                Box::new(client),
                tokio_tungstenite::tungstenite::protocol::Role::Client,
                None,
            )
            .await;
            let peer = tokio::spawn(async move {
                let mut socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    server,
                    tokio_tungstenite::tungstenite::protocol::Role::Server,
                    None,
                )
                .await;
                while let Some(Ok(Message::Text(text))) = socket.next().await {
                    let id = wire::decode_ping(&text).unwrap();
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    if socket
                        .send(Message::Text(wire::encode_pong(id, 0).into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let (observations, mut receiver) = mpsc::channel(16);
            let (_stop, mut cancelled) = watch::channel(false);
            measure(
                Bus::WebSocket(Box::new(socket)),
                Duration::from_millis(80),
                16,
                Instant::now() + Duration::from_millis(100),
                &observations,
                &mut cancelled,
            )
            .await?;
            let mut replies = 0;
            let mut timeouts = 0;
            while let Ok(event) = receiver.try_recv() {
                match event {
                    Observation::Sample { .. } => replies += 1,
                    Observation::Lost {
                        outcome: ProbeOutcome::Timeout,
                        ..
                    } => timeouts += 1,
                    _ => panic!("unexpected outcome"),
                }
            }
            assert_eq!(
                (replies, timeouts),
                if delay == 150 { (2, 0) } else { (0, 2) }
            );
            peer.abort();
        }
        Ok(())
    }

    #[tokio::test]
    async fn websocket_proxy_uses_absolute_form_and_validates_upgrade() -> Result<(), Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _ = crate::crypto::provider().install_default();
        for valid in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let peer = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(stream.read_u8().await.unwrap());
                }
                let head = String::from_utf8(head).unwrap();
                assert!(
                    head.starts_with("GET http://meter.test/ws/ping HTTP/1.1\r\n"),
                    "{head}"
                );
                assert!(
                    head.contains("proxy-authorization: Basic dXNlcjpzZWNyZXQ="),
                    "{head}"
                );
                let key = head
                    .lines()
                    .find_map(|line| line.strip_prefix("sec-websocket-key: "))
                    .unwrap();
                let key = if valid {
                    tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes())
                } else {
                    String::from("invalid")
                };
                stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {key}\r\n\r\n").as_bytes()).await.unwrap();
                if valid {
                    let mut socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                        stream,
                        tokio_tungstenite::tungstenite::protocol::Role::Server,
                        None,
                    )
                    .await;
                    socket.send(Message::Text("pong".into())).await.unwrap();
                    let _ = socket.next().await;
                }
            });
            let mut http = Http::new(false)?;
            http.set_proxy(graphite_meter_net::Proxy::new(
                &format!("http://user:secret@{address}"),
                "",
                "",
            ));
            let (_cancel, mut cancel) = watch::channel(false);
            tokio::time::timeout(Duration::from_secs(5), async {
                let result = connect_ws(&http, "http://meter.test", false, &mut cancel).await;
                if valid {
                    let mut socket = result?.unwrap();
                    assert_eq!(socket.next().await.unwrap()?, Message::Text("pong".into()));
                    socket.close(None).await?;
                } else {
                    assert!(result.is_err());
                }
                peer.await?;
                Ok::<_, Error>(())
            })
            .await??;
        }
        Ok(())
    }
}

//! An owned WebSocket session measures raw RTT on Tokio's monotonic clock.
//! Native CLI grants authenticate the handshake directly: they cannot mint browser tickets.
use crate::{Error, failure::NotReplaced, net::Http};
use futures_util::{SinkExt, StreamExt};
use graphite_meter_core::{
    discovery::{LatencyTarget, LatencyTransport},
    latency::{DeadlineEstimator, ProbeOutcome},
    origin::canonical_origin,
    wire,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig};

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
                rtt, server_handling, ..
            } => ProbeOutcome::Reply {
                // run() bounds duration to the core's signed nanosecond clock range.
                rtt_nanos: rtt.as_nanos() as i64,
                handling_nanos: server_handling.as_nanos() as u64,
            },
            Self::Lost { outcome, .. } => outcome,
        })
    }
}

/// How a session ends: at the stage end in-window probes drain to their deadlines; a stop ends it at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Stop {
    #[default]
    Running,
    Drain,
    Now,
}

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    WebSocket,
    WebTransport,
}

/// The caller must validate this selected origin against its catalogue/preflight.
/// Backpressure is a measurement error, never silently dropped observations.
pub(crate) async fn run_kind(
    http: &Http,
    origin: &str,
    insecure: bool,
    timing: (Duration, Duration, usize),
    observations: mpsc::Sender<Observation>,
    mut cancel: watch::Receiver<Stop>,
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
    // One estimate per stage, as in Go: a redial must not restart at the 250 ms floor.
    let mut estimator = DeadlineEstimator::default();
    loop {
        emit(&observations, Observation::ConnectionBoundary)?;
        let result = measure(
            socket,
            interval,
            window,
            end,
            &mut estimator,
            &observations,
            &mut cancel,
        )
        .await;
        let Err(error) = result else {
            return Ok(());
        };
        if !error.is::<Disconnected>() {
            return Err(error);
        }
        let reconnect_until = (Instant::now() + Duration::from_secs(2)).min(end);
        let mut cause = None;
        socket = loop {
            if Instant::now() >= end {
                return Ok(());
            }
            let attempt =
                tokio::time::timeout_at(reconnect_until, connect(http, origin, insecure, &mut cancel, kind)).await;
            match attempt {
                Ok(Ok(Some(socket))) => break socket,
                Ok(Ok(None)) => return Ok(()),
                Ok(Err(error)) if error.is::<crate::net::AuthRequired>() => return Err(error),
                Ok(Err(error)) => cause = Some(error),
                Err(elapsed) => {
                    cause.get_or_insert_with(|| elapsed.into());
                }
            }
            if Instant::now() >= end {
                return Ok(());
            }
            if Instant::now() >= reconnect_until {
                return Err(NotReplaced("latency channel", cause).into());
            }
            tokio::select! {
                biased;
                () = cancelled(&mut cancel) => return Ok(()),
                () = tokio::time::sleep_until(reconnect_until) => {
                    if reconnect_until == end { return Ok(()); }
                    return Err(NotReplaced("latency channel", cause).into());
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
            Self::WebTransport(session) => (Writer::WebTransport(session.clone()), Reader::WebTransport(session)),
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
            Self::WebTransport(session) => {
                Some(
                    session
                        .recv_datagram()
                        .await
                        .map(|bytes| match String::from_utf8(bytes.to_vec()) {
                            Ok(text) => Message::Text(text.into()),
                            Err(_) => Message::Binary(bytes),
                        }),
                )
            }
        }
    }
}

/// Check the actual latency channel before a run starts. A successful HTTP
/// probe does not establish that QUIC datagrams or WebSocket pings work.
pub(crate) async fn verify(http: &Http, target: &LatencyTarget, insecure: bool) -> Result<Duration, Error> {
    let kind = match target.transport {
        LatencyTransport::WebSocket => Kind::WebSocket,
        LatencyTransport::WebTransport => Kind::WebTransport,
    };
    let attempt = async {
        let (_stop, mut cancel) = watch::channel(Stop::Running);
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
                let sent = Instant::now();
                writer.send(wire::encode_ping(0)).await?;
                let reply = tokio::time::timeout(reply_window, async {
                    loop {
                        match reader.next().await {
                            Some(Ok(Message::Text(text))) => {
                                if wire::decode_pong(&text).is_ok_and(|pong| pong.id == 0) {
                                    return Ok(sent.elapsed());
                                }
                            }
                            Some(Ok(Message::Close(frame))) => {
                                if let Some(ending) = frame.and_then(|frame| {
                                    graphite_meter_core::failure::LaneEnding::from_websocket_code(frame.code.into())
                                }) {
                                    break Err(Box::new(crate::failure::LaneFailure(ending)) as Error);
                                }
                                break Err(Disconnected("latency channel closed before measurement ended").into());
                            }
                            None => {
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
    cancel: &mut watch::Receiver<Stop>,
    kind: Kind,
) -> Result<Option<Bus>, Error> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut backoff = crate::transport::RetryBackoff::default();
    loop {
        let started = Instant::now();
        let error = match connect_once(http, origin, insecure, cancel, kind).await {
            Ok(bus) => return Ok(bus),
            Err(error) => error,
        };
        if crate::failure::reason(error.as_ref(), false) != graphite_meter_core::failure::FailureReason::ServerBusy
            || Instant::now() >= deadline
        {
            return Err(error);
        }
        let wake = (Instant::now() + backoff.delay(error.as_ref(), started)).min(deadline);
        tokio::select! {
            biased;
            () = cancelled(cancel) => return Ok(None),
            () = tokio::time::sleep_until(wake) => {},
        }
        if Instant::now() >= deadline {
            return Err(error);
        }
    }
}

async fn connect_once(
    http: &Http,
    origin: &str,
    insecure: bool,
    cancel: &mut watch::Receiver<Stop>,
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
    cancel: &mut watch::Receiver<Stop>,
) -> Result<Option<Socket>, Error> {
    let origin = canonical_origin(origin)?;
    let target = format!("{origin}/ws/ping");
    let websocket = if let Some(rest) = target.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        format!(
            "ws://{}",
            target.strip_prefix("http://").ok_or("invalid WebSocket origin")?
        )
    };
    let mut request = websocket.into_client_request()?;
    if let Some(authorization) = http.authorization(&target) {
        if insecure {
            return Err("authenticated operation refuses insecure TLS".into());
        }
        request.headers_mut().insert(http::header::AUTHORIZATION, authorization);
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
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(connection.stream)).await?;
        tokio::spawn(driver.with_upgrades());
        let response = sender.send_request(request.map(|()| crate::net::empty())).await?;
        if response.status() != http::StatusCode::SWITCHING_PROTOCOLS {
            http.check_status(&target, response.status(), response.headers())?;
            return Err("WebSocket upgrade was not accepted".into());
        }
        let headers = response.headers();
        let upgrade = headers.get(http::header::UPGRADE).and_then(|value| value.to_str().ok());
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
        let stream: Box<dyn graphite_meter_net::Stream> = Box::new(hyper_util::rt::TokioIo::new(upgraded));
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
    estimator: &mut DeadlineEstimator,
    observations: &mpsc::Sender<Observation>,
    cancel: &mut watch::Receiver<Stop>,
) -> Result<(), Error> {
    let (mut writer, mut reader) = socket.split();
    let mut pending = BTreeMap::<u32, (Instant, Instant)>::new();
    let mut late = BTreeMap::<u32, Instant>::new();
    let mut next_id = 0_u32;
    let mut next_send = Instant::now();
    let mut sending = true;
    let mut expiry = tokio::time::interval(Duration::from_millis(50));
    let result = loop {
        if (!sending || Instant::now() >= end) && pending.is_empty() {
            break Ok(());
        }
        tokio::select! {
            biased;
            stop = stopped(cancel, if sending { Stop::Drain } else { Stop::Now }) => {
                if stop == Stop::Now {
                    break Ok(());
                }
                sending = false;
            }
            _ = expiry.tick() => {
                let now = Instant::now();
                let mut failure = None;
                pending.retain(|&id, (sent, deadline)| {
                    if now < *deadline { return true; }
                    late.insert(id, *sent);
                    if let Err(error) = emit(observations, Observation::Lost { sent: *sent, outcome: ProbeOutcome::Timeout }) {
                        failure = Some(error);
                    }
                    false
                });
                late.retain(|_, sent| now.duration_since(*sent).as_nanos() <= u128::from(DeadlineEstimator::CEIL_NANOS));
                if let Some(error) = failure { break Err(error); }
            }
            () = tokio::time::sleep_until(next_send), if sending && next_send < end => {
                let sent = Instant::now();
                let timeout = Duration::from_nanos(estimator.deadline_nanos());
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
                        if let Some(sent) = late.remove(&pong.id) {
                            estimator.observe(received.saturating_duration_since(sent).as_nanos() as u64);
                            if interval.is_zero() { next_send = received; }
                            continue;
                        }
                        let Some((sent, deadline)) = pending.remove(&pong.id) else { continue };
                        let rtt = received.saturating_duration_since(sent);
                        estimator.observe(rtt.as_nanos() as u64);
                        let observation = if received >= deadline {
                            Observation::Lost { sent, outcome: ProbeOutcome::Timeout }
                        } else {
                            if interval.is_zero() { next_send = received; }
                            Observation::Sample { sent, received, rtt, server_handling: Duration::from_nanos(pong.handling_nanos) }
                        };
                        if let Err(error) = emit(observations, observation) { break Err(error); }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        if let Some(ending) = frame.and_then(|frame| graphite_meter_core::failure::LaneEnding::from_websocket_code(frame.code.into())) {
                            break Err(Box::new(crate::failure::LaneFailure(ending)) as Error);
                        }
                        break Err(Disconnected("latency channel closed before measurement ended").into());
                    }
                    None => break Err(Disconnected("latency channel closed before measurement ended").into()),
                    Some(Err(error)) => {
                        let error = crate::failure::lane_error(error);
                        if error.is::<crate::failure::LaneFailure>() { break Err(error); }
                        break Err(Disconnected("latency channel receive failed").into());
                    }
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
async fn stopped(cancel: &mut watch::Receiver<Stop>, at_least: Stop) -> Stop {
    loop {
        let stop = *cancel.borrow_and_update();
        if stop >= at_least {
            return stop;
        }
        if cancel.changed().await.is_err() {
            return Stop::Now;
        }
    }
}

async fn cancelled(cancel: &mut watch::Receiver<Stop>) {
    stopped(cancel, Stop::Drain).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn busy_upgrade_waits_for_backoff_and_retry_after_before_redial() -> Result<(), Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _ = crate::crypto::provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let http = Http::new(false)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
        let date_lead = Duration::from_millis(500);
        let whole_second = Duration::from_secs((now + date_lead).as_secs() + 1);
        tokio::time::sleep(whole_second - date_lead - now).await;
        let date = httpdate::fmt_http_date(std::time::UNIX_EPOCH + whole_second);
        let refusal = format!(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: {date}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let peer = tokio::spawn(async move {
            let mut attempts = Vec::new();
            let mut minimum = Duration::ZERO;
            for response in [
                Some(refusal.as_str()),
                Some(
                    "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                None,
            ] {
                let (mut stream, _) = listener.accept().await?;
                attempts.push(Instant::now());
                if let Some(response) = response {
                    let mut request = Vec::new();
                    loop {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).await?;
                        request.push(byte[0]);
                        if request.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    if attempts.len() == 1 {
                        minimum = httpdate::parse_http_date(&date)?
                            .duration_since(std::time::SystemTime::now())
                            .unwrap_or_default()
                            .max(Duration::from_millis(300));
                    }
                    stream.write_all(response.as_bytes()).await?;
                } else {
                    let _socket = tokio_tungstenite::accept_async(stream).await?;
                }
            }
            Ok::<_, Error>((attempts, minimum))
        });
        let (_stop, mut cancel) = watch::channel(Stop::Running);
        let bus = connect(&http, &origin, false, &mut cancel, Kind::WebSocket).await?;
        assert!(bus.is_some());
        let (attempts, minimum) = peer.await??;
        assert!(attempts[1] - attempts[0] + Duration::from_millis(20) >= minimum);
        assert!(attempts[2] - attempts[1] >= Duration::from_secs(1));
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn deadlines_learn_from_late_replies_and_drain_at_the_stage_boundary() -> Result<(), Error> {
        for (delay, interval, duration, expected) in
            [(150, 80, 100, (2, 0)), (300, 80, 100, (0, 2)), (400, 500, 1600, (3, 1))]
        {
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
            let (_stop, mut cancelled) = watch::channel(Stop::Running);
            measure(
                Bus::WebSocket(Box::new(socket)),
                Duration::from_millis(interval),
                16,
                Instant::now() + Duration::from_millis(duration),
                &mut DeadlineEstimator::default(),
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
            assert_eq!((replies, timeouts), expected, "{delay} ms echo");
            peer.abort();
        }
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_settles_pending_probes_without_waiting_for_their_deadlines() -> Result<(), Error> {
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
            while let Some(Ok(_)) = socket.next().await {}
        });
        let (observations, mut receiver) = mpsc::channel(64);
        let (stop, mut cancel) = watch::channel(Stop::Running);
        let mut estimator = DeadlineEstimator::default();
        estimator.observe(9_000_000_000);
        let started = Instant::now();
        let session = measure(
            Bus::WebSocket(Box::new(socket)),
            Duration::from_millis(100),
            16,
            started + Duration::from_secs(60),
            &mut estimator,
            &observations,
            &mut cancel,
        );
        let stop_later = async {
            tokio::time::sleep(Duration::from_millis(350)).await;
            stop.send_replace(Stop::Now);
        };
        let (result, ()) = tokio::join!(session, stop_later);
        result?;
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        let mut unresolved = 0;
        while let Ok(event) = receiver.try_recv() {
            assert!(matches!(
                event,
                Observation::Lost {
                    outcome: ProbeOutcome::Unresolved,
                    ..
                }
            ));
            unresolved += 1;
        }
        assert_eq!(unresolved, 4);
        peer.abort();
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
                assert!(head.starts_with("GET http://meter.test/ws/ping HTTP/1.1\r\n"), "{head}");
                assert!(head.contains("proxy-authorization: Basic dXNlcjpzZWNyZXQ="), "{head}");
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
            let (_cancel, mut cancel) = watch::channel(Stop::Running);
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

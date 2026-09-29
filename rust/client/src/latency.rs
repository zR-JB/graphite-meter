//! An owned WebSocket session measures raw RTT on Tokio's monotonic clock.
//! Native CLI grants authenticate the handshake directly: they cannot mint browser tickets.
use crate::{
    Error,
    failure::Failure,
    net::Http,
    transport::{REDIAL_WINDOW, TRANSFER_RETRY_BACKOFF, restore},
};
use futures_util::{SinkExt, StreamExt};
use graphite_meter_core::{
    discovery::{LatencyTarget, LatencyTransport},
    failure::LaneEnding,
    latency::{DeadlineEstimator, ProbeOutcome},
    origin::canonical_origin,
    route::Route,
    wire,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
use tokio_tungstenite::tungstenite::{
    Message,
    client::IntoClientRequest,
    protocol::{CloseFrame, WebSocketConfig},
};

type Socket = tokio_tungstenite::WebSocketStream<Box<dyn graphite_meter_net::Stream>>;

#[derive(Clone, Copy, Debug)]
pub enum Observation {
    ConnectionBoundary,
    Sample {
        sent: Instant,
        rtt: Duration,
        handling_nanos: u64,
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
                rtt, handling_nanos, ..
            } => ProbeOutcome::Reply {
                // run() bounds duration to the core's signed nanosecond clock range.
                rtt_nanos: rtt.as_nanos() as i64,
                handling_nanos,
            },
            Self::Lost { outcome, .. } => outcome,
        })
    }
}

/// How far a session's stage has come: once its window opens, the window's end bounds a lost
/// channel's redial; at the stage end in-window probes drain to their deadlines; a stop ends it
/// at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Stop {
    #[default]
    Running,
    /// The measured window opened and ends at this instant.
    Window(Instant),
    Drain,
    Now,
}

/// The caller must validate this selected origin against its catalogue/preflight.
/// A probe is skipped while the observation queue lacks room for its outcome, so none is dropped.
pub(crate) async fn run(
    http: &Http,
    target: &LatencyTarget,
    timing: (Duration, Duration, usize),
    observations: mpsc::Sender<Observation>,
    mut cancel: watch::Receiver<Stop>,
) -> Result<(), Error> {
    let (interval, duration, window) = timing;
    if duration.is_zero() || duration.as_nanos() > i64::MAX as u128 {
        return Err("latency interval and bounded duration must be positive".into());
    }
    let first = dial(http, target, Instant::now() + REDIAL_WINDOW);
    let mut socket = tokio::select! {
        biased;
        _ = stopped(&mut cancel, Stop::Drain) => return Ok(()),
        bus = first => bus?,
    };
    let end = Instant::now()
        .checked_add(duration)
        .ok_or("latency duration exceeds clock range")?;
    let mut ledger = Ledger::default();
    loop {
        observations
            .send(Observation::ConnectionBoundary)
            .await
            .map_err(|_| "latency observation consumer closed")?;
        let opened = Instant::now();
        let result = measure(socket, interval, window, end, &mut ledger, &observations, &mut cancel).await;
        let Err(error) = result else {
            return Ok(());
        };
        // As Go's measureLatency (latency.go:244-264), a lost channel is dialled again whatever
        // ended it but a revoked grant, once a probe was ever answered (probeLedger.interrupt);
        // once the window has ended, the loss ends the session.
        if !lost(&error) || !ledger.answered {
            return Err(error);
        }
        let now = Instant::now();
        let window_end = match *cancel.borrow() {
            Stop::Running => end,
            Stop::Window(window_end) => window_end.min(end),
            Stop::Drain | Stop::Now => return Ok(()),
        };
        if now >= window_end {
            return Ok(());
        }
        // The window's end cuts a redial short and fails it (probeLedger.bound, latency.go:256),
        // so only a stop ends one. A channel lost as it opened waits as a lane that failed at
        // once does (transfer.go:100-103), so a server that ends each channel at once is never
        // dialled in a tight loop.
        let bound = (now + REDIAL_WINDOW).min(window_end);
        let pause = if opened.elapsed() < TRANSFER_RETRY_BACKOFF {
            TRANSFER_RETRY_BACKOFF
        } else {
            Duration::ZERO
        };
        let redial = async {
            tokio::time::sleep_until((now + pause).min(bound)).await;
            dial(http, target, bound).await
        };
        socket = tokio::select! {
            biased;
            _ = stopped(&mut cancel, Stop::Now) => return Ok(()),
            bus = redial => match bus {
                Ok(bus) => bus,
                // The session's own end, which the window's precedes, ends it.
                Err(_) if Instant::now() >= end => return Ok(()),
                Err(error) => return Err(error),
            },
        };
    }
}

/// What a session learns across its channels, as Go's probeLedger keeps it: one deadline
/// estimate, so a redial does not restart at the 250 ms floor, and whether any probe was answered.
#[derive(Default)]
struct Ledger {
    estimator: DeadlineEstimator,
    answered: bool,
}

impl Ledger {
    /// A probe's reply, in time or late, after `rtt`.
    fn observe(&mut self, rtt: Duration) {
        self.estimator.observe(rtt.as_nanos() as u64);
        self.answered = true;
    }
}

/// A channel the server ended with any lane ending but a revoked grant, or that failed
/// (failure.go:83-95), which Go's measureLatency dials again.
fn lost(error: &Error) -> bool {
    match error.downcast_ref() {
        Some(Failure::Lane(ending)) => *ending != LaneEnding::Revoked,
        other => matches!(other, Some(Failure::Disconnected(_))),
    }
}

/// Go's redialPingBus (latency.go:124-132): the channel dialled until `deadline`, paced as Go's
/// restore paces it.
async fn dial(http: &Http, target: &LatencyTarget, deadline: Instant) -> Result<Bus, Error> {
    restore("latency channel", deadline, || {
        connect(http, &target.base_url, target.transport)
    })
    .await
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
            Self::WebTransport(session) => match session.recv_datagram().await {
                Ok(bytes) => Some(Ok(match String::from_utf8(bytes.to_vec()) {
                    Ok(text) => Message::Text(text.into()),
                    Err(_) => Message::Binary(bytes),
                })),
                // A session the server closed reads as a WebSocket closed with the same lane ending.
                Err(_) => session.ending().map(|ending| {
                    Ok(Message::Close(Some(CloseFrame {
                        code: ending.websocket_code().into(),
                        reason: ending.reason().into(),
                    })))
                }),
            },
        }
    }
}

/// Check the actual latency channel before a run starts. A successful HTTP
/// probe does not establish that QUIC datagrams or WebSocket pings work.
pub(crate) async fn verify(http: &Http, target: &LatencyTarget) -> Result<Duration, Error> {
    let attempt = async {
        // One dial, as Go's verifyLatency (latency.go:85-97).
        let (mut writer, mut reader) = connect(http, &target.base_url, target.transport).await?.split();
        let result = async {
            let reply_window = match target.transport {
                LatencyTransport::WebTransport => Duration::from_millis(750),
                LatencyTransport::WebSocket => Duration::from_secs(3),
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
                            Some(Ok(Message::Close(frame))) => return Err(closed(frame)),
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
                    Err(_) if target.transport == LatencyTransport::WebTransport => continue,
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
async fn connect(http: &Http, origin: &str, transport: LatencyTransport) -> Result<Bus, Error> {
    Ok(match transport {
        LatencyTransport::WebSocket => Bus::WebSocket(Box::new(connect_ws(http, origin).await?)),
        LatencyTransport::WebTransport => {
            let target = crate::net::url(&canonical_origin(origin)?, Route::WtPing, &[]);
            let session = crate::webtransport::Session::dial(http, &target, Duration::from_secs(10)).await?;
            Bus::WebTransport(Arc::new(session))
        }
    })
}

async fn connect_ws(http: &Http, origin: &str) -> Result<Socket, Error> {
    let origin = canonical_origin(origin)?;
    let target = crate::net::url(&origin, Route::Ping, &[]);
    // The canonical origin is http:// or https://, so the channel is ws:// or wss://.
    let websocket = format!("ws{}", target.strip_prefix("http").ok_or("invalid WebSocket origin")?);
    let mut request = websocket.into_client_request()?;
    http.authorize(&target, request.headers_mut())?;
    // TLS 1.2 and 1.3 as for throughput and in Go; only QUIC requires 1.3.
    let tls = match origin.starts_with("https://") {
        true => Some(crate::tls::tcp(http.insecure, crate::tls::Alpn::Http1).await?),
        false => None,
    };
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
            Route::Ping.path().parse()?
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
    tokio::time::timeout(Duration::from_secs(10), connection).await?
}

/// The lane ending a close frame names, or a channel that closed before the stage ended.
fn closed(frame: Option<CloseFrame>) -> Error {
    match frame.and_then(|frame| LaneEnding::from_websocket_code(frame.code.into())) {
        Some(ending) => Box::new(Failure::Lane(ending)),
        None => Failure::Disconnected("latency channel closed before measurement ended").into(),
    }
}

async fn measure(
    socket: Bus,
    interval: Duration,
    window: usize,
    end: Instant,
    ledger: &mut Ledger,
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
            () = due(next_send), if sending && next_send < end => {
                let sent = Instant::now();
                let timeout = Duration::from_nanos(ledger.estimator.deadline_nanos());
                next_send = if interval.is_zero() {
                    sent + timeout
                } else {
                    // Go's ticker keeps its schedule: the next probe is its first tick after this one.
                    let missed = sent.saturating_duration_since(next_send).as_nanos() / interval.as_nanos();
                    let ticks = u32::try_from(missed + 1).unwrap_or(u32::MAX);
                    next_send.checked_add(interval.saturating_mul(ticks)).unwrap_or(end)
                };
                if pending.len() >= window || observations.capacity() <= pending.len() { continue; }
                let id = next_id;
                let Some(next) = next_id.checked_add(1) else { break Err("latency probe identifier exhausted".into()); };
                next_id = next;
                pending.insert(id, (sent, sent + timeout));
                if !matches!(tokio::time::timeout(Duration::from_secs(1), writer.send(wire::encode_ping(id))).await, Ok(Ok(()))) {
                    pending.remove(&id);
                    if let Err(error) = emit(observations, Observation::Lost { sent, outcome: ProbeOutcome::SendFailure }) { break Err(error); }
                    break Err(Failure::Disconnected("latency channel send failed").into());
                }
            }
            message = reader.next() => {
                let received = Instant::now();
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(pong) = wire::decode_pong(&text) else { continue };
                        // As Go's reader, every pong sends the next reply-driven probe, late or not.
                        if interval.is_zero() { next_send = received; }
                        if let Some(sent) = late.remove(&pong.id) {
                            ledger.observe(received.saturating_duration_since(sent));
                            continue;
                        }
                        let Some((sent, deadline)) = pending.remove(&pong.id) else { continue };
                        let rtt = received.saturating_duration_since(sent);
                        ledger.observe(rtt);
                        let observation = if received >= deadline {
                            Observation::Lost { sent, outcome: ProbeOutcome::Timeout }
                        } else {
                            Observation::Sample { sent, rtt, handling_nanos: pong.handling_nanos }
                        };
                        if let Err(error) = emit(observations, observation) { break Err(error); }
                    }
                    Some(Ok(Message::Close(frame))) => break Err(closed(frame)),
                    None => break Err(Failure::Disconnected("latency channel closed before measurement ended").into()),
                    Some(Err(_)) => break Err(Failure::Disconnected("latency channel receive failed").into()),
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

/// Ready at once when `at` has passed: tokio rounds every wait up to the next millisecond.
async fn due(at: Instant) {
    if at > Instant::now() {
        tokio::time::sleep_until(at).await;
    }
}

fn emit(observations: &mpsc::Sender<Observation>, observation: Observation) -> Result<(), Error> {
    observations
        .try_send(observation)
        .map_err(|_| "latency observation consumer closed or fell behind".into())
}
async fn stopped(cancel: &mut watch::Receiver<Stop>, at_least: Stop) -> Stop {
    cancel
        .wait_for(|stop| *stop >= at_least)
        .await
        .map_or(Stop::Now, |stop| *stop)
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
        let target = LatencyTarget {
            base_url: origin,
            transport: LatencyTransport::WebSocket,
        };
        dial(&http, &target, Instant::now() + REDIAL_WINDOW).await?;
        let (attempts, minimum) = peer.await??;
        assert!(attempts[1] - attempts[0] + Duration::from_millis(20) >= minimum);
        assert!(attempts[2] - attempts[1] >= Duration::from_secs(1));
        Ok(())
    }

    /// The first dial is tried again as Go's measureLatency tries it (latency.go:141), whatever
    /// the failure: here the server refuses its first upgrade outright.
    #[tokio::test]
    async fn the_first_dial_is_tried_again_after_any_failure() -> Result<(), Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _ = crate::crypto::provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let target = LatencyTarget {
            base_url: format!("http://{}", listener.local_addr()?),
            transport: LatencyTransport::WebSocket,
        };
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let _ = stream.read(&mut [0; 4096]).await?;
            stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await?;
            let (stream, _) = listener.accept().await?;
            echo(tokio_tungstenite::accept_async(stream).await?, Duration::ZERO).await;
            Ok::<_, Error>(())
        });
        let (observations, mut observed) = mpsc::channel(64);
        let (_stop, cancel) = watch::channel(Stop::Running);
        let timing = (Duration::from_millis(50), Duration::from_millis(300), 16);
        let measured = run(&Http::new(false)?, &target, timing, observations, cancel).await;
        peer.abort();
        measured?;
        assert!(replies(&mut observed) > 0);
        Ok(())
    }

    /// A lost channel redials for 2 s at Go's pace (latency.go:124-132, transfer.go:95-104):
    /// 500 ms after each refusal, where 100 ms made some twenty dials.
    #[tokio::test]
    async fn a_lost_channel_redials_at_gos_pace() -> Result<(), Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _ = crate::crypto::provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let target = LatencyTarget {
            base_url: format!("http://{}", listener.local_addr()?),
            transport: LatencyTransport::WebSocket,
        };
        let redials = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = redials.clone();
        // The first channel answers a probe and closes; the redials find upgrades refused.
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            end_after_an_answer(&mut tokio_tungstenite::accept_async(stream).await?, LaneEnding::Idle).await?;
            for _ in 0..32 {
                let (mut stream, _) = listener.accept().await?;
                seen.lock().unwrap().push(Instant::now());
                let _ = stream.read(&mut [0; 4096]).await?;
                stream
                    .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await?;
            }
            Ok::<_, Error>(())
        });
        let (observations, _observed) = mpsc::channel(64);
        let (_stop, cancel) = watch::channel(Stop::Running);
        let timing = (Duration::from_millis(50), Duration::from_secs(10), 16);
        let lost = run(&Http::new(false)?, &target, timing, observations, cancel).await;
        peer.abort();
        let redials = redials.lock().unwrap();
        assert!(lost.is_err_and(|error| error.to_string().contains("not replaced")));
        assert!((3..=5).contains(&redials.len()), "{} redials", redials.len());
        for pair in redials.windows(2) {
            assert!(pair[1] - pair[0] >= crate::transport::TRANSFER_RETRY_BACKOFF);
        }
        Ok(())
    }

    /// A close frame naming `ending`.
    fn close_frame(ending: LaneEnding) -> CloseFrame {
        CloseFrame {
            code: ending.websocket_code().into(),
            reason: ending.reason().into(),
        }
    }

    /// Answers the first probe on `socket`, then ends it with `ending`.
    async fn end_after_an_answer<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
        socket: &mut tokio_tungstenite::WebSocketStream<S>,
        ending: LaneEnding,
    ) -> Result<(), Error> {
        if let Some(Ok(Message::Text(text))) = socket.next().await {
            let pong = wire::encode_pong(wire::decode_ping(&text)?, 0);
            socket.send(Message::Text(pong.into())).await?;
        }
        Ok(socket.close(Some(close_frame(ending))).await?)
    }

    /// The samples among `observed`.
    fn replies(observed: &mut mpsc::Receiver<Observation>) -> usize {
        let mut replies = 0;
        while let Ok(observation) = observed.try_recv() {
            replies += usize::from(matches!(observation, Observation::Sample { .. }));
        }
        replies
    }

    /// A session whose first channel answered and then `ending` ended, after `dials` dials: its
    /// channel was dialled again and answered, unless the ending revoked the grant, which asks
    /// for sign-in after the one dial.
    fn check_ending(result: Result<(), Error>, ending: LaneEnding, dials: usize, replies: usize) {
        let revoked = ending == LaneEnding::Revoked;
        match result {
            Ok(()) => assert!(
                !revoked && dials > 1 && replies > 1,
                "{ending:?}: {dials} dials, {replies} replies"
            ),
            Err(error) => {
                let reason = crate::failure::reason(error.as_ref(), false);
                let sign_in = reason == graphite_meter_core::failure::FailureReason::SignInRequired;
                assert!(
                    revoked && dials == 1 && sign_in,
                    "{ending:?}: {error} after {dials} dials"
                );
            }
        }
    }

    /// As Go's TestMeasureLatencyFailsPromptlyOnAnUnprovenBus (latency_test.go:134-148): a channel
    /// that never answered a probe fails when it is lost, dialled once (probeLedger.interrupt).
    #[tokio::test]
    async fn a_channel_that_never_answered_fails_when_it_is_lost() -> Result<(), Error> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _ = crate::crypto::provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let target = LatencyTarget {
            base_url: format!("http://{}", listener.local_addr()?),
            transport: LatencyTransport::WebSocket,
        };
        let dials = Arc::new(AtomicUsize::new(0));
        let dialled = dials.clone();
        let peer = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                dialled.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await {
                    let _ = socket.close(Some(close_frame(LaneEnding::Finished))).await;
                }
            }
        });
        let (observations, _observed) = mpsc::channel(64);
        let (_stop, cancel) = watch::channel(Stop::Running);
        let timing = (Duration::from_millis(50), Duration::from_secs(1), 16);
        let (http, started) = (Http::new(false)?, Instant::now());
        let session = run(&http, &target, timing, observations, cancel);
        let lost = tokio::time::timeout(Duration::from_secs(5), session).await?;
        peer.abort();
        let (lasted, dials) = (started.elapsed(), dials.load(Ordering::SeqCst));
        assert!(lost.is_err() && dials == 1, "{lost:?} after {dials} dials");
        assert!(lasted < Duration::from_millis(500), "failed after {lasted:?}");
        Ok(())
    }

    /// A channel that answered, then is lost each time it opens, is dialled again at a lane's
    /// pace, 500 ms after a quick loss (transfer.go:95-104), never in a tight loop.
    #[tokio::test]
    async fn a_channel_lost_as_it_opens_is_dialled_again_at_a_lanes_pace() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let target = LatencyTarget {
            base_url: format!("http://{}", listener.local_addr()?),
            transport: LatencyTransport::WebSocket,
        };
        let dials = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = dials.clone();
        // Only the first channel answers a probe before it ends.
        let peer = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let first = {
                    let mut dials = seen.lock().unwrap();
                    dials.push(Instant::now());
                    dials.len() == 1
                };
                let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                    continue;
                };
                let _ = match first {
                    true => end_after_an_answer(&mut socket, LaneEnding::Idle).await,
                    false => socket
                        .close(Some(close_frame(LaneEnding::Idle)))
                        .await
                        .map_err(Into::into),
                };
            }
        });
        let (observations, _observed) = mpsc::channel(64);
        let (_stop, cancel) = watch::channel(Stop::Running);
        let timing = (Duration::from_millis(50), Duration::from_millis(1600), 16);
        let http = Http::new(false)?;
        let session = run(&http, &target, timing, observations, cancel);
        let result = tokio::time::timeout(Duration::from_secs(5), session).await;
        peer.abort();
        result??;
        let dials = dials.lock().unwrap();
        assert!((3..=5).contains(&dials.len()), "{} dials", dials.len());
        for pair in dials.windows(2) {
            assert!(
                pair[1] - pair[0] >= crate::transport::TRANSFER_RETRY_BACKOFF,
                "{dials:?}"
            );
        }
        Ok(())
    }

    /// As Go's TestLaneEndingsNameTheirReason (latency_test.go:67-110): a channel the server ends
    /// with a lane ending after an answer is dialled again, unless the ending revokes the grant.
    #[tokio::test]
    async fn a_lane_ending_is_dialled_again_unless_it_revokes_the_grant() -> Result<(), Error> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _ = crate::crypto::provider().install_default();
        for ending in [
            LaneEnding::Idle,
            LaneEnding::Lifetime,
            LaneEnding::Shutdown,
            LaneEnding::Revoked,
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let target = LatencyTarget {
                base_url: format!("http://{}", listener.local_addr()?),
                transport: LatencyTransport::WebSocket,
            };
            let dials = Arc::new(AtomicUsize::new(0));
            let dialled = dials.clone();
            // The first channel answers a probe and ends; later ones echo.
            let peer = tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                        continue;
                    };
                    if dialled.fetch_add(1, Ordering::SeqCst) > 0 {
                        tokio::spawn(echo(socket, Duration::ZERO));
                    } else {
                        let _ = end_after_an_answer(&mut socket, ending).await;
                    }
                }
            });
            let (observations, mut observed) = mpsc::channel(256);
            let (_stop, cancel) = watch::channel(Stop::Running);
            let timing = (Duration::from_millis(20), Duration::from_millis(1500), 16);
            let result = run(&Http::new(false)?, &target, timing, observations, cancel).await;
            peer.abort();
            check_ending(result, ending, dials.load(Ordering::SeqCst), replies(&mut observed));
        }
        Ok(())
    }

    /// A channel the server ends once the window ended, while the last probes drain, ends the
    /// session cleanly: Go leaves a loss after the window alone (latency.go:252).
    #[tokio::test]
    async fn a_lane_ending_while_the_last_probes_drain_ends_the_session() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let target = LatencyTarget {
            base_url: format!("http://{}", listener.local_addr()?),
            transport: LatencyTransport::WebSocket,
        };
        let (holding, held) = tokio::sync::oneshot::channel();
        let (ended, window_ended) = tokio::sync::oneshot::channel();
        // The first probe is answered and the second held until the window ended.
        let peer = tokio::spawn(async move {
            let mut socket = tokio_tungstenite::accept_async(listener.accept().await?.0).await?;
            for held in [false, true] {
                let Some(Ok(Message::Text(text))) = socket.next().await else {
                    return Err("no probe".into());
                };
                if !held {
                    let pong = wire::encode_pong(wire::decode_ping(&text)?, 0);
                    socket.send(Message::Text(pong.into())).await?;
                }
            }
            let _ = holding.send(());
            let _: Result<(), _> = window_ended.await;
            Ok::<_, Error>(socket.close(Some(close_frame(LaneEnding::Lifetime))).await?)
        });
        let (observations, _observed) = mpsc::channel(64);
        let (stop, cancel) = watch::channel(Stop::Running);
        let timing = (Duration::from_millis(50), Duration::from_secs(10), 16);
        let http = Http::new(false)?;
        let session = run(&http, &target, timing, observations, cancel);
        let end_window = async {
            let _ = held.await;
            stop.send_replace(Stop::Drain);
            let _ = ended.send(());
        };
        let (result, ()) =
            tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(session, end_window) }).await?;
        peer.abort();
        result
    }

    /// A WebTransport session the server closes with a lane ending's code is dialled again as a
    /// WebSocket is, unless the ending revokes the grant (latency.go:33-47, failure.go:83-95).
    #[tokio::test]
    async fn a_webtransport_lane_ending_is_dialled_again_unless_it_revokes_the_grant() -> Result<(), Error> {
        use graphite_meter_http3::{server::Connection, webtransport::Session};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _ = crate::crypto::provider().install_default();
        for ending in [LaneEnding::Lifetime, LaneEnding::Revoked] {
            let (endpoint, origin) = crate::fixtures::h3_endpoint()?;
            let dials = Arc::new(AtomicUsize::new(0));
            let dialled = dials.clone();
            // The first session answers a probe and ends; later ones echo.
            let server = tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                while let Some(incoming) = endpoint.accept().await {
                    let first = dialled.fetch_add(1, Ordering::SeqCst) == 0;
                    connections.spawn(async move {
                        let mut connection = Connection::new(incoming.await?, None);
                        let (_, stream) = connection.next().await?.ok_or("no CONNECT")?.resolve().await?;
                        let serve = async {
                            let session = Session::accept(stream, http::HeaderMap::new()).await?;
                            while let Some(ping) = session.read_datagram().await {
                                let pong = wire::encode_pong(wire::decode_ping(std::str::from_utf8(&ping)?)?, 0);
                                session.send_datagram(pong.as_bytes())?;
                                if first {
                                    session.close(ending.webtransport_code(), ending.reason()).await;
                                }
                            }
                            Ok::<_, Error>(())
                        };
                        let (served, ()) =
                            tokio::join!(serve, async { while let Ok(Some(_)) = connection.next().await {} });
                        served
                    });
                }
            });
            let target = LatencyTarget {
                base_url: origin,
                transport: LatencyTransport::WebTransport,
            };
            let (observations, mut observed) = mpsc::channel(256);
            let (_stop, cancel) = watch::channel(Stop::Running);
            let timing = (Duration::from_millis(20), Duration::from_millis(1500), 16);
            let result = run(&Http::new(true)?, &target, timing, observations, cancel).await;
            server.abort();
            check_ending(result, ending, dials.load(Ordering::SeqCst), replies(&mut observed));
        }
        Ok(())
    }

    async fn echo<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
        mut socket: tokio_tungstenite::WebSocketStream<S>,
        delay: Duration,
    ) {
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let id = wire::decode_ping(&text).unwrap();
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if socket
                .send(Message::Text(wire::encode_pong(id, 0).into()))
                .await
                .is_err()
            {
                break;
            }
        }
    }

    type Peer = tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>;

    /// A latency channel over an in-memory link, whose far end `peer` serves.
    async fn linked<F>(peer: impl FnOnce(Peer) -> F + Send + 'static) -> (Bus, tokio::task::JoinHandle<()>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        use tokio_tungstenite::tungstenite::protocol::Role;
        let (client, server) = tokio::io::duplex(4096);
        let socket = Socket::from_raw_socket(Box::new(client), Role::Client, None).await;
        let peer = tokio::spawn(async move { peer(Peer::from_raw_socket(server, Role::Server, None).await).await });
        (Bus::WebSocket(Box::new(socket)), peer)
    }

    async fn outcomes(bus: Bus, interval: u64, duration: u64) -> Result<(usize, usize), Error> {
        let (observations, mut receiver) = mpsc::channel(16);
        let (_stop, mut cancelled) = watch::channel(Stop::Running);
        measure(
            bus,
            Duration::from_millis(interval),
            16,
            Instant::now() + Duration::from_millis(duration),
            &mut Ledger::default(),
            &observations,
            &mut cancelled,
        )
        .await?;
        let (mut replies, mut timeouts) = (0, 0);
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
        Ok((replies, timeouts))
    }

    #[tokio::test(start_paused = true)]
    async fn replies_count_until_their_deadline_after_the_stage_boundary() -> Result<(), Error> {
        for (delay, expected) in [(150, (2, 0)), (300, (0, 2))] {
            let (bus, peer) = linked(move |server| echo(server, Duration::from_millis(delay))).await;
            assert_eq!(outcomes(bus, 80, 100).await?, expected, "{delay} ms echo");
            peer.abort();
        }
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn reply_driven_probes_follow_replies_until_the_queue_is_full() -> Result<(), Error> {
        let (bus, peer) = linked(|server| echo(server, Duration::ZERO)).await;
        let (observations, mut receiver) = mpsc::channel(64);
        let (_stop, mut cancel) = watch::channel(Stop::Running);
        // Off a millisecond boundary, where tokio's timer rounds a wait up.
        tokio::time::advance(Duration::from_micros(500)).await;
        let started = Instant::now();
        let end = started + Duration::from_millis(10);
        measure(
            bus,
            Duration::ZERO,
            4,
            end,
            &mut Ledger::default(),
            &observations,
            &mut cancel,
        )
        .await?;
        let mut samples = 0;
        while let Ok(Observation::Sample { sent, .. }) = receiver.try_recv() {
            assert_eq!(sent, started, "a probe waited for a timer");
            samples += 1;
        }
        assert_eq!(samples, 64);
        peer.abort();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn a_fixed_cadence_keeps_its_schedule_like_gos_ticker() -> Result<(), Error> {
        let (bus, peer) = linked(|server| echo(server, Duration::ZERO)).await;
        let (observations, mut receiver) = mpsc::channel(256);
        let (_stop, mut cancel) = watch::channel(Stop::Running);
        // Off a millisecond boundary, where tokio's timer rounds each wait up.
        tokio::time::advance(Duration::from_micros(500)).await;
        let started = Instant::now();
        measure(
            bus,
            Duration::from_millis(80),
            16,
            started + Duration::from_secs(10),
            &mut Ledger::default(),
            &observations,
            &mut cancel,
        )
        .await?;
        let mut sent = Vec::new();
        while let Ok(Observation::Sample { sent: at, .. }) = receiver.try_recv() {
            sent.push(at - started);
        }
        // A probe every 80 ms from the first: 125 in ten seconds.
        assert_eq!(sent.len(), 125, "last probe at {:?}", sent.last());
        peer.abort();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn every_pong_sends_the_next_reply_driven_probe() -> Result<(), Error> {
        // Probe 0 is answered after 10 ms. Probe 1 is answered after 260 ms: 10 ms past its deadline,
        // before the expiry sweep at 300 ms. Later probes get no answer.
        let (bus, peer) = linked(|mut socket| async move {
            let mut due = None;
            loop {
                tokio::select! {
                    message = socket.next() => {
                        let Some(Ok(Message::Text(text))) = message else { break };
                        let id = wire::decode_ping(&text).unwrap();
                        if let Some(delay) = [10, 260].get(id as usize) {
                            due = Some((id, Instant::now() + Duration::from_millis(*delay)));
                        }
                    }
                    () = tokio::time::sleep_until(due.map_or_else(Instant::now, |(_, at)| at)), if due.is_some() => {
                        let (id, _) = due.take().unwrap();
                        if socket.send(Message::Text(wire::encode_pong(id, 0).into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        })
        .await;
        let (observations, mut receiver) = mpsc::channel(64);
        let (_stop, mut cancel) = watch::channel(Stop::Running);
        let started = Instant::now();
        measure(
            bus,
            Duration::ZERO,
            4,
            started + Duration::from_millis(400),
            &mut Ledger::default(),
            &observations,
            &mut cancel,
        )
        .await?;
        let mut sent = Vec::new();
        while let Ok(observation) = receiver.try_recv() {
            if let Observation::Sample { sent: at, .. } | Observation::Lost { sent: at, .. } = observation {
                sent.push((at - started).as_millis());
            }
        }
        sent.sort_unstable();
        // Probe 2 is the backup at probe 1's deadline; probe 3 answers probe 1's late pong.
        assert_eq!(sent, [0, 10, 260, 270]);
        peer.abort();
        Ok(())
    }

    #[tokio::test]
    async fn a_late_reply_over_a_real_socket_extends_later_deadlines() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            echo(
                tokio_tungstenite::accept_async(stream).await?,
                Duration::from_millis(400),
            )
            .await;
            Ok::<_, Error>(())
        });
        let bus = connect(&Http::new(false)?, &origin, LatencyTransport::WebSocket).await?;
        assert_eq!(outcomes(bus, 500, 1600).await?, (3, 1));
        peer.abort();
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_settles_pending_probes_without_waiting_for_their_deadlines() -> Result<(), Error> {
        let (bus, peer) = linked(|mut socket| async move { while let Some(Ok(_)) = socket.next().await {} }).await;
        let (observations, mut receiver) = mpsc::channel(64);
        let (stop, mut cancel) = watch::channel(Stop::Running);
        let mut ledger = Ledger::default();
        ledger.observe(Duration::from_secs(9));
        let started = Instant::now();
        let session = measure(
            bus,
            Duration::from_millis(100),
            16,
            started + Duration::from_secs(60),
            &mut ledger,
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

    /// Latency over WSS takes the TLS 1.2 that throughput takes, as Go's WebSocket client does.
    #[tokio::test]
    async fn secure_websocket_latency_accepts_tls12_like_throughput() -> Result<(), Error> {
        let _ = crate::crypto::provider().install_default();
        let tls = crate::fixtures::server_tls(&[&rustls::version::TLS12], &[])?;
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("https://{}", listener.local_addr()?);
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let stream = acceptor.accept(stream).await?;
            echo(tokio_tungstenite::accept_async(stream).await?, Duration::ZERO).await;
            Ok::<_, Error>(())
        });
        let pong = tokio::time::timeout(Duration::from_secs(5), async {
            let mut socket = connect_ws(&Http::new(true)?, &origin).await?;
            socket.send(Message::Text(wire::encode_ping(7).into())).await?;
            match socket.next().await.ok_or("latency channel closed")?? {
                Message::Text(text) => Ok::<_, Error>(wire::decode_pong(&text)?.id),
                other => Err(format!("unexpected {other:?}").into()),
            }
        })
        .await;
        peer.abort();
        assert_eq!(pong??, 7);
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
            tokio::time::timeout(Duration::from_secs(5), async {
                let result = connect_ws(&http, "http://meter.test").await;
                if valid {
                    let mut socket = result?;
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

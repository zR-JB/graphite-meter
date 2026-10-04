//! An owned WebSocket session measures raw RTT on Tokio's monotonic clock.
//! Native CLI grants authenticate the handshake directly: they cannot mint browser tickets.
use crate::{
    Error,
    failure::Failure,
    net::Http,
    transport::{REDIAL_WINDOW, restore, retry_pause},
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
use std::{collections::BTreeMap, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig};

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
    let mut socket = tokio::select! {
        biased;
        _ = stopped(&mut cancel, Stop::Drain) => return Ok(()),
        bus = dial(http, target, Instant::now() + REDIAL_WINDOW) => bus?,
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
        let Err(error) = measure(socket, interval, window, end, &mut ledger, &observations, &mut cancel).await else {
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
        // dialled in a tight loop; a wait that would reach the bound is skipped, and the channel
        // dialled at once, as Go dials every loss.
        let bound = (now + REDIAL_WINDOW).min(window_end);
        let paced = now + retry_pause(opened);
        let paced = if paced < bound { paced } else { now };
        let redial = async {
            tokio::time::sleep_until(paced).await;
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

/// A latency channel: a WebSocket, or a WebTransport session's datagrams.
enum Bus {
    WebSocket(Box<Socket>),
    WebTransport(Box<crate::webtransport::Session>),
}
impl Bus {
    async fn send(&mut self, text: String) -> Result<(), Error> {
        match self {
            Self::WebSocket(socket) => socket.send(Message::Text(text.into())).await?,
            Self::WebTransport(session) => session.send_datagram(text.as_bytes()).await?,
        }
        Ok(())
    }
    async fn close(self) {
        if let Self::WebSocket(mut socket) = self {
            let _ = tokio::time::timeout(Duration::from_millis(250), (*socket).close(None)).await;
        }
    }
    /// The lane ending the server closed a WebTransport session with, once it has.
    fn ending(&self) -> Option<LaneEnding> {
        match self {
            Self::WebTransport(session) => session.ending(),
            Self::WebSocket(_) => None,
        }
    }
    async fn next(&mut self) -> Option<Result<Message, Error>> {
        match self {
            Self::WebSocket(socket) => socket.next().await.map(|result| result.map_err(Into::into)),
            Self::WebTransport(session) => match session.recv_datagram().await {
                Ok(bytes) => Some(Ok(match String::from_utf8(bytes.to_vec()) {
                    Ok(text) => Message::Text(text.into()),
                    Err(_) => Message::Binary(bytes),
                })),
                // A session the server closed ends with its lane ending, as a WebSocket's close frame does.
                Err(_) => session.ending().map(|ending| Err(Failure::Lane(ending).into())),
            },
        }
    }
}

/// Check the actual latency channel before a run starts. A successful HTTP
/// probe does not establish that QUIC datagrams or WebSocket pings work.
pub(crate) async fn verify(http: &Http, target: &LatencyTarget) -> Result<Duration, Error> {
    let attempt = async {
        // One dial, as Go's verifyLatency (latency.go:85-97).
        let mut bus = connect(http, &target.base_url, target.transport).await?;
        let result = async {
            let reply_window = match target.transport {
                LatencyTransport::WebTransport => Duration::from_millis(750),
                LatencyTransport::WebSocket => Duration::from_secs(3),
            };
            loop {
                let sent = Instant::now();
                bus.send(wire::encode_ping(0)).await?;
                let reply = tokio::time::timeout(reply_window, async {
                    loop {
                        match bus.next().await {
                            Some(Ok(Message::Text(text))) => {
                                if wire::decode_pong(&text).is_ok_and(|pong| pong.id == 0) {
                                    return Ok(sent.elapsed());
                                }
                            }
                            Some(Ok(Message::Close(frame))) => return Err(closed(frame)),
                            None => return Err("latency channel closed before replying".into()),
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
        bus.close().await;
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
            Bus::WebTransport(Box::new(session))
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
    // A message up to Go's default read limit, 32 KiB, is read and one that is not a pong skipped
    // (latency.go:33-47); a longer one ends the channel, as there.
    let config = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(4096)
        .max_message_size(Some(32 * 1024))
        .max_frame_size(Some(32 * 1024));
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
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
        };
        let connection = header("connection").split(',');
        if !header("upgrade").eq_ignore_ascii_case("websocket")
            || !connection
                .map(str::trim)
                .any(|token| token.eq_ignore_ascii_case("upgrade"))
            || header("sec-websocket-accept") != key
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
    mut bus: Bus,
    interval: Duration,
    window: usize,
    end: Instant,
    ledger: &mut Ledger,
    observations: &mpsc::Sender<Observation>,
    cancel: &mut watch::Receiver<Stop>,
) -> Result<(), Error> {
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
                if !matches!(tokio::time::timeout(Duration::from_secs(1), bus.send(wire::encode_ping(id))).await, Ok(Ok(()))) {
                    pending.remove(&id);
                    if let Err(error) = emit(observations, Observation::Lost { sent, outcome: ProbeOutcome::SendFailure }) { break Err(error); }
                    // Only the session's first probe ends it (latency.go:218-220), with the ending the server closed
                    // it with, if any; a later one's failure is counted and the reader decides (latency.go:265-268).
                    if id == 0 && !ledger.answered {
                        break Err(bus.ending().map_or_else(|| Failure::Disconnected("latency channel send failed").into(), |ending| Failure::Lane(ending).into()));
                    }
                }
            }
            message = bus.next() => {
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
                    Some(Err(error)) if error.is::<Failure>() => break Err(error),
                    Some(Err(_)) => break Err(Failure::Disconnected("latency channel receive failed").into()),
                    _ => {}
                }
            }
        }
    };
    let outcome = ProbeOutcome::Unresolved;
    for (_, (sent, _)) in pending {
        emit(observations, Observation::Lost { sent, outcome })?;
    }
    bus.close().await;
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
    use std::sync::Arc;
    use tokio::{io::AsyncWriteExt, net::TcpListener};

    async fn websocket_listener() -> Result<(TcpListener, LatencyTarget), Error> {
        let (listener, base_url) = crate::fixtures::listener().await?;
        let transport = LatencyTransport::WebSocket;
        Ok((listener, LatencyTarget { base_url, transport }))
    }

    #[tokio::test]
    async fn upgrade_refusals_retry_with_busy_backoff_and_retry_after() -> Result<(), Error> {
        let (listener, target) = websocket_listener().await?;
        let http = Http::new(false)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?;
        let date_lead = Duration::from_millis(500);
        let whole_second = Duration::from_secs((now + date_lead).as_secs() + 1);
        tokio::time::sleep(whole_second - date_lead - now).await;
        let date = httpdate::fmt_http_date(std::time::UNIX_EPOCH + whole_second);
        let empty = "Content-Length: 0\r\nConnection: close\r\n\r\n";
        let peer = tokio::spawn(async move {
            let mut attempts = Vec::new();
            let mut minimum = Duration::ZERO;
            for response in [
                Some(format!(
                    "HTTP/1.1 429 Too Many Requests\r\nRetry-After: {date}\r\n{empty}"
                )),
                Some(format!("HTTP/1.1 503 Service Unavailable\r\nRetry-After: 1\r\n{empty}")),
                Some(format!("HTTP/1.1 404 Not Found\r\n{empty}")),
                None,
            ] {
                let (mut stream, _) = listener.accept().await?;
                attempts.push(Instant::now());
                if let Some(response) = response {
                    crate::fixtures::read_head(&mut stream).await?;
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
        dial(&http, &target, Instant::now() + REDIAL_WINDOW + Duration::from_secs(1)).await?;
        let (attempts, minimum) = peer.await??;
        assert!(attempts[1] - attempts[0] + Duration::from_millis(20) >= minimum);
        assert!(attempts[2] - attempts[1] >= Duration::from_secs(1));
        Ok(())
    }

    /// A close frame naming `ending`.
    fn close_frame(ending: LaneEnding) -> CloseFrame {
        CloseFrame {
            code: ending.websocket_code().into(),
            reason: ending.reason().into(),
        }
    }

    /// As Go's measureLatency (latency.go:244-264): a channel the server ends is dialled again
    /// unless the ending revokes the grant (TestLaneEndingsNameTheirReason, latency_test.go:67-110)
    /// or no probe was ever answered (probeLedger.interrupt, latency_test.go:134-148); once the
    /// window has ended, a loss ends the session cleanly (latency.go:252). A channel lost as it
    /// opened, with less of the window left than the pause after such a loss, is dialled again at once.
    #[tokio::test]
    async fn a_lost_channel_is_dialled_again_as_go_dials_it() -> Result<(), Error> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // The first channel answers a probe if `answered`, holds the next and ends with `ending`,
        // as the stage drains if `drained`; later ones echo. The window ends `window` ms in.
        for (ending, answered, drained, window, dials, outcome) in [
            (LaneEnding::Idle, true, false, None, 2, "Ok(())"),
            (LaneEnding::Lifetime, true, false, None, 2, "Ok(())"),
            (LaneEnding::Shutdown, true, false, None, 2, "Ok(())"),
            (LaneEnding::Revoked, true, false, None, 1, "Err(Lane(Revoked))"),
            (LaneEnding::Finished, false, false, None, 1, "Err(Lane(Finished))"),
            (LaneEnding::Lifetime, true, true, None, 1, "Ok(())"),
            (LaneEnding::Idle, true, false, Some(450), 2, "Ok(())"),
        ] {
            let (listener, target) = websocket_listener().await?;
            let window = window.map(|ms| Stop::Window(Instant::now() + Duration::from_millis(ms)));
            let (stop, cancel) = watch::channel(window.unwrap_or_default());
            let dialled = Arc::new(AtomicUsize::new(0));
            let counted = dialled.clone();
            let peer = tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let mut socket = tokio_tungstenite::accept_async(stream).await?;
                    if counted.fetch_add(1, Ordering::SeqCst) > 0 {
                        tokio::spawn(echo(socket, Duration::ZERO));
                        continue;
                    }
                    for answer in [answered, false] {
                        if let Some(Ok(Message::Text(text))) = socket.next().await
                            && answer
                        {
                            let pong = wire::encode_pong(wire::decode_ping(&text)?, 0);
                            socket.send(Message::Text(pong.into())).await?;
                        }
                    }
                    if drained {
                        stop.send_replace(Stop::Drain);
                    }
                    socket.close(Some(close_frame(ending))).await?;
                }
                Ok::<_, Error>(())
            });
            let (observations, _observed) = mpsc::channel(256);
            let timing = (Duration::from_millis(20), Duration::from_secs(1), 16);
            let result = run(&Http::new(false)?, &target, timing, observations, cancel).await;
            peer.abort();
            let seen = (format!("{result:?}"), dialled.load(Ordering::SeqCst));
            assert_eq!(seen, (outcome.into(), dials), "{ending:?}");
        }
        Ok(())
    }

    /// A WebTransport session the server closes as revoked reads as that lane ending, as a
    /// WebSocket's close frame does, so it asks for sign-in and is not dialled again
    /// (latency.go:33-47, failure.go:83-95). Here the close arrives before a due probe: the session's
    /// first probe reports its ending, and a later one's failure is counted while the reader decides,
    /// as Go's (latency.go:218-220, 265-268).
    #[tokio::test]
    async fn a_webtransport_session_closed_as_revoked_is_not_dialled_again() -> Result<(), Error> {
        let mut seen = Vec::new();
        for answered in [false, true] {
            let (endpoint, origin) = crate::fixtures::h3_endpoint()?;
            let server = tokio::spawn(crate::fixtures::webtransport_peer(endpoint, |session| async move {
                let revoked = LaneEnding::Revoked;
                session.close(revoked.webtransport_code(), revoked.reason()).await;
                std::future::pending().await
            }));
            let bus = connect(&Http::new(true)?, &origin, LatencyTransport::WebTransport).await?;
            let Bus::WebTransport(session) = &bus else {
                unreachable!()
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                while !session.is_closed() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await?;
            let mut ledger = Ledger::default();
            if answered {
                ledger.observe(Duration::from_millis(1));
            }
            let (observations, _observed) = mpsc::channel(64);
            let (_stop, mut cancel) = watch::channel(Stop::Running);
            let end = Instant::now() + Duration::from_secs(5);
            let interval = Duration::from_millis(20);
            let result = measure(bus, interval, 16, end, &mut ledger, &observations, &mut cancel).await;
            server.abort();
            seen.push((format!("{result:?}"), result.as_ref().err().is_some_and(lost)));
        }
        assert_eq!(seen, vec![(String::from("Err(Lane(Revoked))"), false); 2]);
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

    /// Keep the outcome queue undrained until measurement ends, including its backpressure.
    async fn collect(
        bus: Bus,
        interval: u64,
        duration: u64,
        capacity: usize,
        window: usize,
    ) -> Result<(Instant, mpsc::Receiver<Observation>), Error> {
        let (observations, receiver) = mpsc::channel(capacity);
        let (_stop, mut cancel) = watch::channel(Stop::Running);
        let started = Instant::now();
        let (interval, end) = (
            Duration::from_millis(interval),
            started + Duration::from_millis(duration),
        );
        measure(
            bus,
            interval,
            window,
            end,
            &mut Ledger::default(),
            &observations,
            &mut cancel,
        )
        .await?;
        Ok((started, receiver))
    }

    async fn outcomes(bus: Bus, interval: u64, duration: u64) -> Result<(usize, usize), Error> {
        let (_, mut receiver) = collect(bus, interval, duration, 16, 16).await?;
        let (mut replies, mut timeouts) = (0, 0);
        while let Ok(event) = receiver.try_recv() {
            match event.outcome() {
                Some(ProbeOutcome::Reply { .. }) => replies += 1,
                Some(ProbeOutcome::Timeout) => timeouts += 1,
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
        // Off a millisecond boundary, where Tokio rounds a timer wait up.
        tokio::time::advance(Duration::from_micros(500)).await;
        let (started, mut receiver) = collect(bus, 0, 10, 64, 4).await?;
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
        // Off a millisecond boundary, where Tokio rounds a timer wait up.
        tokio::time::advance(Duration::from_micros(500)).await;
        let (started, mut receiver) = collect(bus, 80, 10_000, 256, 16).await?;
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
    async fn a_stop_settles_pending_probes_without_waiting_for_their_deadlines() -> Result<(), Error> {
        let (bus, peer) = linked(|mut socket| async move { while let Some(Ok(_)) = socket.next().await {} }).await;
        let (observations, mut receiver) = mpsc::channel(64);
        let (stop, mut cancel) = watch::channel(Stop::Running);
        let mut ledger = Ledger::default();
        ledger.observe(Duration::from_secs(9));
        let started = Instant::now();
        let (interval, end) = (Duration::from_millis(100), started + Duration::from_secs(60));
        let session = measure(bus, interval, 16, end, &mut ledger, &observations, &mut cancel);
        let stop_later = async {
            tokio::time::sleep(Duration::from_millis(350)).await;
            stop.send_replace(Stop::Now);
        };
        let (result, ()) = tokio::join!(session, stop_later);
        result?;
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        let mut unresolved = 0;
        while let Ok(event) = receiver.try_recv() {
            assert!(matches!(event.outcome(), Some(ProbeOutcome::Unresolved)));
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
        for valid in [false, true] {
            let (listener, origin) = crate::fixtures::listener().await?;
            let peer = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let head = crate::fixtures::read_head(&mut stream).await.unwrap();
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
            let proxy = origin.replacen("//", "//user:secret@", 1);
            http.set_proxy(graphite_meter_net::Proxy::new(&proxy, "", ""));
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

//! A CONNECT task owns every application lane; dropping it cancels all session IO.
//! The connection advertises no WT_INITIAL_* settings, so session flow control
//! is not negotiated. QUIC flow control and the local lane limit remain active.
use super::{body::write_reply, http3::H3Reply, quic::ReceiveCredit, *};
use crate::{
    timeouts::{PROGRESS_HEARTBEAT, WT_ANSWER, WT_REFUSAL_LINGER, WT_VERIFY_LINGER},
    upload::{UploadLane, UploadSubscription},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_core::{
    failure::{LaneEnding, UploadRefusal},
    route::{self, Route},
    wire::{self, UploadProgress},
};
use graphite_meter_http3::{
    RequestStream,
    webtransport::{RecvStream, Session},
};
use tokio::time::Instant;

// A ready datagram send need not yield. Bound each burst so sibling sessions
// still get executor time without paying a scheduler round-trip per packet.
const DATAGRAM_YIELD_BATCH: usize = 16;
/// How often a session looks for idleness and retries funding a refused upload window.
const SESSION_TICK: Duration = Duration::from_millis(100);
/// Go's wtDatagramPayload: each download datagram carries this much.
const DATAGRAM_BYTES: u64 = 1000;
/// A download stream lane writes at most this much at a time.
const LANE_WRITE_BYTES: u64 = 16 * 1024;
type Failure = Box<dyn std::error::Error + Send + Sync>;
type Activity = Arc<Mutex<Instant>>;

fn touch(activity: &Activity) {
    *lock(activity) = Instant::now();
}

impl HttpServer {
    pub(super) async fn serve_webtransport(
        self: Arc<Self>,
        request: Request<()>,
        stream: RequestStream,
        credit: ReceiveCredit,
        peer: SocketAddr,
    ) -> io::Result<()> {
        // A CONNECT has no body to end.
        let (request, route) = match self.gate(request, Accepted::quic(peer), true) {
            Ok(passed) => passed,
            Err(response) => return answer(stream, *response).await,
        };
        let (request, lease) = request.into_parts();
        // Under authentication a session lives only as long as its lease.
        if self.auth.is_some() && lease.is_none() {
            return answer(stream, self.harden(text_response(StatusCode::FORBIDDEN))).await;
        }
        let Some(route) = route else {
            return answer(stream, self.harden(text_response(StatusCode::NOT_FOUND))).await;
        };
        let owner = self.owner(&request, lease.as_ref(), peer);
        let _permit = match self.admit(route, &owner) {
            Ok(permit) => permit,
            Err(refusal) => return answer(stream, self.harden(*refusal)).await,
        };
        let _admitted = credit.work().admit();
        let lifetime = if route.admission() == route::Admission::Session {
            self.config.max_session_duration
        } else {
            self.config.max_operation_duration
        };
        let deadline = Instant::now() + lifetime;
        // Like every secure response under authentication, as Go's Enforce sets them before routing.
        let mut headers = http::HeaderMap::new();
        if self.auth.is_some() {
            crate::auth::pages::harden(&mut headers, true);
        }
        let session = tokio::select! {
            biased;
            _ = lease_ended(lease.clone()) => return Ok(()),
            session = Session::accept(stream, headers) => session.map_err(io::Error::other)?,
        };
        let activity = Arc::new(Mutex::new(Instant::now()));
        // This scope owns every route future, so its lanes drop before the session's close is sent.
        let ending = {
            let serving = async {
                match route {
                    Route::WtDownload => serve_download(&self, &session, &request, &activity).await,
                    Route::WtUpload => serve_upload(&self, &session, &request, owner, credit, &activity).await,
                    _ => serve_ping(&session, &activity).await,
                }
            };
            tokio::pin!(serving);
            let mut tick = tokio::time::interval(SESSION_TICK);
            loop {
                tokio::select! {
                    biased;
                    _ = session.closed() => break LaneEnding::Finished,
                    _ = lease_ended(lease.clone()) => break LaneEnding::Revoked,
                    _ = tick.tick() => {
                        if Instant::now().duration_since(*lock(&activity)) >= IDLE_BOUND {
                            break LaneEnding::Idle;
                        }
                    }
                    _ = tokio::time::sleep_until(deadline) => break LaneEnding::Lifetime,
                    ending = &mut serving => break ending,
                }
            }
        };
        session.close(ending.webtransport_code(), ending.reason()).await;
        Ok(())
    }
}

/// Go's WTPing: peer datagrams count as activity, even when they name no valid probe.
async fn serve_ping(session: &Session, activity: &Activity) -> LaneEnding {
    loop {
        tokio::select! {
            biased;
            payload = session.read_datagram() => {
                let Some(payload) = payload else { return LaneEnding::Finished };
                touch(activity);
                if let Some(reply) = crate::ping::reply(&payload) {
                    let _ = session.send_datagram(reply.as_bytes());
                }
            }
            incoming = session.accept_uni() => {
                let Some(_incoming) = incoming else { return LaneEnding::Finished };
            }
        }
    }
}

/// Go's WTDownload: outgoing stream lanes or a datagram flood, with peer-opened streams refused by dropping them.
async fn serve_download(
    server: &HttpServer,
    session: &Session,
    request: &Request<()>,
    activity: &Activity,
) -> LaneEnding {
    let datagrams = query(request, "datagrams").is_some_and(|value| datagram_mode(&value));
    let count = download_bytes(request);
    let transfer = async {
        if count == 0 {
            tokio::time::sleep(WT_VERIFY_LINGER).await;
        } else if datagrams {
            let _ = datagram_flood(session, count, server.download_block.clone(), &server.download_meter).await;
        } else {
            let streams = query(request, "streams")
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(1)
                .min(wire::MAX_WEBTRANSPORT_STREAMS as i64);
            let mut lanes = FuturesUnordered::new();
            for _ in 0..streams {
                lanes.push(download_lane(
                    session,
                    count,
                    server.download_block.clone(),
                    activity.clone(),
                    &server.download_meter,
                ));
            }
            while lanes.next().await.is_some() {}
        }
        LaneEnding::Finished
    };
    tokio::pin!(transfer);
    loop {
        tokio::select! {
            biased;
            ending = &mut transfer => return ending,
            payload = session.read_datagram() => {
                let Some(_) = payload else { return LaneEnding::Finished };
                if datagrams {
                    touch(activity);
                }
            }
            incoming = session.accept_uni() => {
                let Some(_incoming) = incoming else { return LaneEnding::Finished };
            }
        }
    }
}

/// Go's WTUpload: bounded incoming lanes and progress writers, with receiver-owned counts and client credit.
async fn serve_upload(
    server: &HttpServer,
    session: &Session,
    request: &Request<()>,
    owner: Owner,
    credit: ReceiveCredit,
    activity: &Activity,
) -> LaneEnding {
    let id = query(request, "id").unwrap_or_default();
    let subscription = server.uploads.subscribe(&id, &owner);
    let refused = subscription.is_err();
    let mut awaiting_credit = !refused && !credit.fund(owner.client_keys());
    let mut controls = FuturesUnordered::new();
    controls.push(progress(session, subscription));
    let mut streams = FuturesUnordered::new();
    let mut datagram_lane = query(request, "datagrams")
        .is_some_and(|value| datagram_mode(&value))
        .then(|| server.uploads.begin(&id, &owner).ok())
        .flatten();
    let datagram_finished = datagram_lane.as_ref().map(UploadLane::finished);
    tokio::pin!(datagram_finished);
    let mut settle = None;
    let mut tick = tokio::time::interval(SESSION_TICK);
    loop {
        tokio::select! {
            biased;
            _ = tick.tick(), if awaiting_credit => awaiting_credit = !credit.fund(owner.client_keys()),
            _ = tokio::time::sleep_until(settle.unwrap_or_else(Instant::now)), if settle.is_some() => {
                return LaneEnding::Finished;
            }
            _ = async { datagram_finished.as_mut().as_pin_mut().expect("active datagram lane").await }, if datagram_lane.is_some() => {
                datagram_lane = None;
            }
            Some(_) = streams.next(), if !streams.is_empty() => {}
            Some(_) = controls.next(), if !controls.is_empty() => {
                if refused && settle.is_none() {
                    settle = Some(Instant::now() + WT_REFUSAL_LINGER);
                }
            }
            payload = session.read_datagram() => {
                let Some(payload) = payload else { return LaneEnding::Finished };
                if let Some(lane) = &mut datagram_lane {
                    lane.record(payload.len());
                    touch(activity);
                }
            }
            incoming = session.accept_uni() => {
                let Some(incoming) = incoming else { return LaneEnding::Finished };
                if streams.len() < wire::MAX_WEBTRANSPORT_STREAMS {
                    match server.uploads.begin(&id, &owner) {
                        Ok(lane) => streams.push(upload_lane(incoming, lane, activity.clone())),
                        Err(error) if controls.len() < 2 => controls.push(progress(session, Err(error))),
                        Err(_) => {}
                    }
                }
            }
        }
    }
}

/// The answer to a CONNECT that opens no session, within its own bound.
async fn answer(stream: RequestStream, response: Response<ResponseBody>) -> io::Result<()> {
    let (mut send, _receive) = stream.split();
    tokio::time::timeout(WT_ANSWER, write_reply(&mut H3Reply::new(&mut send), response, false))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
}

fn datagram_mode(value: &str) -> bool {
    let value = value.trim();
    if let Ok(number) = value.parse::<i64>() {
        return number != 0;
    }
    !matches!(value.to_ascii_lowercase().as_str(), "false" | "off" | "no")
}

/// Repeats `count` bytes of `block` in datagrams until the session ends.
async fn datagram_flood(
    session: &Session,
    count: u64,
    block: Bytes,
    meter: &crate::meter::Meter,
) -> Result<(), Failure> {
    let full_size = count.min(DATAGRAM_BYTES).min(block.len() as u64) as usize;
    let tail_size = (count % full_size as u64) as usize;
    let mut full = session.prepare_datagram(&block[..full_size])?;
    let mut tail = (tail_size > 0)
        .then(|| session.prepare_datagram(&block[..tail_size]))
        .transpose()?;
    let transfer = meter.open();
    let mut since_yield = 0;
    loop {
        let mut remaining = count;
        while remaining > 0 {
            let size = remaining.min(full_size as u64) as usize;
            let datagram = if size == full_size {
                &mut full
            } else {
                tail.as_mut().expect("partial datagram")
            };
            datagram.send_wait().await?;
            if let Some(transfer) = &transfer {
                transfer.record(size);
            }
            remaining -= size as u64;
            since_yield += 1;
            if since_yield == DATAGRAM_YIELD_BATCH {
                since_yield = 0;
                tokio::task::yield_now().await;
            }
        }
    }
}

async fn download_lane(
    session: &Session,
    count: u64,
    block: Bytes,
    activity: Activity,
    meter: &crate::meter::Meter,
) -> Result<(), Failure> {
    loop {
        let mut stream = session.open_uni().await?;
        let transfer = meter.open();
        let mut remaining = count;
        while remaining > 0 {
            let size = remaining.min(LANE_WRITE_BYTES).min(block.len() as u64) as usize;
            match stream.write_chunk(block.slice(..size)).await {
                Ok(()) => {}
                Err(error) if remaining == count => return Err(error.into()),
                Err(_) => break,
            }
            if let Some(transfer) = &transfer {
                transfer.record(size);
            }
            remaining -= size as u64;
            touch(&activity);
        }
        if remaining == 0 {
            stream.finish()?;
        }
        tokio::task::yield_now().await;
    }
}

async fn upload_lane(mut stream: RecvStream, mut lane: UploadLane, activity: Activity) -> Result<(), Failure> {
    while let Ok(Ok(Some(chunk))) = tokio::time::timeout(IDLE_BOUND, stream.read_chunk()).await {
        lane.record(chunk.len());
        touch(&activity);
    }
    Ok(())
}

async fn progress(session: &Session, subscription: Result<UploadSubscription, UploadRefusal>) -> Result<(), Failure> {
    let mut stream = session.open_uni().await?;
    let mut subscription = match subscription {
        Ok(subscription) => subscription,
        Err(refusal) => {
            let event = UploadProgress::Error {
                message: refusal.message().into(),
                code: refusal.name().into(),
            };
            stream
                .write_all(format!("{}\n", wire::encode_upload_progress(&event)?).as_bytes())
                .await?;
            return Ok(stream.finish()?);
        }
    };
    loop {
        let message = tokio::select! {
            event = subscription.next() => { let Some(event) = event else { break; }; format!("{}\n", wire::encode_upload_progress(&event)?) },
            _ = tokio::time::sleep(PROGRESS_HEARTBEAT) => "\n".to_owned(),
        };
        stream.write_all(message.as_bytes()).await?;
    }
    Ok(stream.finish()?)
}

//! A CONNECT task owns every application lane; dropping it cancels all session IO.
//! The connection advertises no WT_INITIAL_* settings, so session flow control
//! is not negotiated. QUIC flow control and the local lane limit remain active.
use super::{body::write_reply, http3::H3Reply, quic::ReceiveCredit, *};
use crate::{
    timeouts::{PROGRESS_HEARTBEAT, WT_ANSWER, WT_REFUSAL_LINGER, WT_VERIFY_LINGER},
    upload::{UploadLane, UploadStore, UploadSubscription},
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
type Lane<'a> = Pin<Box<dyn Future<Output = Result<(), Failure>> + Send + 'a>>;
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
        let accepted = Accepted {
            peer,
            tls: true,
            topology: topology::QUIC.topology,
        };
        // A CONNECT has no body to end.
        let (request, Passed { route, lease, .. }) = match self.gate(request, accepted, true) {
            Ok(passed) => passed,
            Err(response) => return answer(stream, *response).await,
        };
        // Under authentication a session lives only as long as its lease.
        if self.auth.is_some() && lease.is_none() {
            return answer(stream, self.harden(text_response(StatusCode::FORBIDDEN))).await;
        }
        let Some(route) = route else {
            return answer(stream, self.harden(text_response(StatusCode::NOT_FOUND))).await;
        };
        let request = match request {
            Checked::Public(request) => request,
            Checked::Authorized(authorized) => authorized.into_parts().0,
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
        let mut lanes = match route {
            Route::WtDownload => Lanes::download(&self, &session, &request),
            Route::WtUpload => Lanes::upload(&self, &session, &request, owner, credit),
            _ => Lanes::new(route, &request),
        };
        let ending = lanes.run(&session, lease, deadline).await;
        drop(lanes);
        session.close(ending.webtransport_code(), ending.reason()).await;
        Ok(())
    }
}

/// A session's lanes and what its route does with the peer's datagrams and streams: Go's WTPing, WTDownload and
/// WTUpload over one event loop.
struct Lanes<'a> {
    route: Route,
    /// Download or upload lanes.
    streams: FuturesUnordered<Lane<'a>>,
    /// Upload progress, and the error records of refused lanes.
    controls: FuturesUnordered<Lane<'a>>,
    /// When the session's lanes last moved.
    activity: Activity,
    /// A download floods datagrams, or an upload receives them as one more lane.
    datagrams: bool,
    /// When an establish-only download, or an upload refused at connect, closes.
    settle: Option<Instant>,
    upload: Option<Upload>,
    /// The upload was refused at connect.
    refused: bool,
    /// The upload's receive window waits for its client's credit.
    awaiting_credit: bool,
    datagram_lane: Option<UploadLane>,
    /// Resolves once the datagram lane's upload is finished.
    datagram_finished: Pin<Box<dyn Future<Output = ()> + Send>>,
}

/// An upload session's aggregate, for the lanes its peer opens and the credit its window waits for.
struct Upload {
    store: UploadStore,
    id: String,
    owner: Owner,
    credit: ReceiveCredit,
}

impl<'a> Lanes<'a> {
    /// Go's WTPing: the peer's datagrams are echoed probes.
    fn new(route: Route, request: &Request<()>) -> Self {
        Self {
            route,
            streams: FuturesUnordered::new(),
            controls: FuturesUnordered::new(),
            activity: Arc::new(Mutex::new(Instant::now())),
            datagrams: query(request, "datagrams").is_some_and(|value| datagram_mode(&value)),
            settle: None,
            upload: None,
            refused: false,
            awaiting_credit: false,
            datagram_lane: None,
            datagram_finished: Box::pin(std::future::pending()),
        }
    }

    /// Go's WTDownload: `streams` stream lanes, or one datagram flood, each of `bytes`; `bytes=0` only establishes.
    fn download(server: &'a HttpServer, session: &'a Session, request: &Request<()>) -> Self {
        let mut lanes = Self::new(Route::WtDownload, request);
        let count = download_bytes(request);
        if count == 0 {
            lanes.settle = Some(Instant::now() + WT_VERIFY_LINGER);
        } else if lanes.datagrams {
            let block = server.download_block.clone();
            lanes
                .streams
                .push(Box::pin(datagram_flood(session, count, block, &server.download_meter)));
        } else {
            let streams = query(request, "streams")
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(1)
                .min(wire::MAX_WEBTRANSPORT_STREAMS as i64);
            for _ in 0..streams {
                lanes.streams.push(Box::pin(download_lane(
                    session,
                    count,
                    server.download_block.clone(),
                    lanes.activity.clone(),
                    &server.download_meter,
                )));
            }
        }
        lanes
    }

    /// Go's WTUpload: progress on a server stream, each peer stream a lane, and its datagrams one more.
    fn upload(
        server: &'a HttpServer,
        session: &'a Session,
        request: &Request<()>,
        owner: Owner,
        credit: ReceiveCredit,
    ) -> Self {
        let mut lanes = Self::new(Route::WtUpload, request);
        let id = query(request, "id").unwrap_or_default();
        let subscription = server.uploads.subscribe(&id, &owner);
        lanes.refused = subscription.is_err();
        lanes.awaiting_credit = !lanes.refused && !credit.fund(owner.client_keys());
        lanes.controls.push(Box::pin(progress(session, subscription)));
        if lanes.datagrams {
            lanes.datagram_lane = server.uploads.begin(&id, &owner).ok();
        }
        if let Some(lane) = &lanes.datagram_lane {
            lanes.datagram_finished = Box::pin(lane.finished());
        }
        lanes.upload = Some(Upload {
            store: server.uploads.clone(),
            id,
            owner,
            credit,
        });
        lanes
    }

    /// Runs the session until the peer, the lease, idleness, the lifetime or the route ends it.
    async fn run(&mut self, session: &'a Session, lease: Option<AuthLease>, deadline: Instant) -> LaneEnding {
        let mut tick = tokio::time::interval(SESSION_TICK);
        loop {
            tokio::select! {
                biased;
                // The peer ended the session, or the connection's shutdown did.
                _ = session.closed() => return LaneEnding::Finished,
                _ = lease_ended(lease.clone()) => return LaneEnding::Revoked,
                _ = tick.tick() => {
                    self.fund();
                    let last = *lock(&self.activity);
                    if Instant::now().duration_since(last) >= IDLE_BOUND {
                        return LaneEnding::Idle;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => return LaneEnding::Lifetime,
                _ = tokio::time::sleep_until(self.settle.unwrap_or(deadline)), if self.settle.is_some() => {
                    return LaneEnding::Finished;
                }
                _ = &mut self.datagram_finished, if self.datagram_lane.is_some() => self.datagram_lane = None,
                Some(_) = self.streams.next(), if !self.streams.is_empty() => {
                    // A download ends with its last lane; later client
                    // streams may replace finished upload lanes.
                    if self.route == Route::WtDownload && self.streams.is_empty() {
                        return LaneEnding::Finished;
                    }
                }
                Some(_) = self.controls.next(), if !self.controls.is_empty() => {
                    if self.refused && self.settle.is_none() {
                        self.settle = Some(Instant::now() + WT_REFUSAL_LINGER);
                    }
                }
                payload = session.read_datagram() => {
                    let Some(payload) = payload else { return LaneEnding::Finished };
                    self.datagram(session, &payload);
                }
                incoming = session.accept_uni() => {
                    // A dropped stream is refused as a cancelled lane.
                    let Some(incoming) = incoming else { return LaneEnding::Finished };
                    self.stream(session, incoming);
                }
            }
        }
    }

    /// An upload whose window waited for its client's credit asks again.
    fn fund(&mut self) {
        if self.awaiting_credit
            && let Some(upload) = &self.upload
        {
            self.awaiting_credit = !upload.credit.fund(upload.owner.client_keys());
        }
    }

    /// A datagram from the peer. A route without a datagram lane gets no idle credit for it.
    fn datagram(&mut self, session: &Session, payload: &[u8]) {
        match self.route {
            Route::WtPing => {
                touch(&self.activity);
                if let Some(reply) = crate::ping::reply(payload) {
                    let _ = session.send_datagram(reply.as_bytes());
                }
            }
            Route::WtDownload if self.datagrams => touch(&self.activity),
            Route::WtUpload => {
                if let Some(lane) = &mut self.datagram_lane {
                    lane.record(payload.len());
                    touch(&self.activity);
                }
            }
            _ => {}
        }
    }

    /// A stream the peer opened: an upload's lane within the lane cap. A lane the aggregate refuses gets an error
    /// record, while at most one other is being written.
    fn stream(&mut self, session: &'a Session, incoming: RecvStream) {
        let Some(upload) = &self.upload else { return };
        if self.streams.len() >= wire::MAX_WEBTRANSPORT_STREAMS {
            return;
        }
        match upload.store.begin(&upload.id, &upload.owner) {
            Ok(lane) => self
                .streams
                .push(Box::pin(upload_lane(incoming, lane, self.activity.clone()))),
            Err(error) if self.controls.len() < 2 => self.controls.push(Box::pin(progress(session, Err(error)))),
            Err(_) => {}
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
    let transfer = meter.open();
    let mut since_yield = 0;
    loop {
        let mut remaining = count;
        while remaining > 0 {
            let size = remaining.min(DATAGRAM_BYTES).min(block.len() as u64) as usize;
            session.send_datagram_wait(&block[..size]).await?;
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

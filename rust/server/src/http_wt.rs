//! A CONNECT task owns every application lane; dropping it cancels all session IO.
//! The connection advertises no WT_INITIAL_* settings, so session flow control
//! is not negotiated. QUIC flow control and the local lane limit remain active.
use super::{http_quic::ReceiveCredit, *};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_core::{
    failure::LaneEnding,
    route::Route,
    wire::{self, UploadProgress},
};
use graphite_meter_http3::{
    self as http3, RequestStream,
    webtransport::{RecvStream, Session},
};
use tokio::time::Instant;

const MAX_LANES: usize = 16;
// A ready datagram send need not yield. Bound each burst so sibling sessions
// still get executor time without paying a scheduler round-trip per packet.
const DATAGRAM_YIELD_BATCH: usize = 16;
const IDLE: Duration = Duration::from_secs(30);
const REFUSAL_LINGER: Duration = Duration::from_secs(2);
type Failure = Box<dyn std::error::Error + Send + Sync>;
type Lane<'a> = Pin<Box<dyn Future<Output = Result<(), Failure>> + Send + 'a>>;
type Activity = Arc<Mutex<Instant>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionRoute {
    Ping,
    Download,
    Upload,
}

fn touch(activity: &Activity) {
    *activity.lock().expect("WT activity poisoned") = Instant::now();
}

impl HttpServer {
    pub(super) async fn serve_webtransport(
        self: Arc<Self>,
        request: Request<()>,
        stream: RequestStream,
        credit: ReceiveCredit,
        peer: SocketAddr,
    ) -> Result<(), http3::Error> {
        if let Some(response) = self.validate_request(&request, true) {
            return answer(stream, response).await;
        }
        let listener = Listener {
            ui: false,
            webtransport: true,
        };
        let connection = Connection {
            peer,
            tls: true,
            listener,
        };
        let (request, lease) = if let Some(auth) = &self.auth {
            match auth.policy().authorize(request, connection) {
                Ok(guard) => {
                    let (request, authorization, _, _) = guard.into_parts();
                    let Authorization::Authenticated(lease) = authorization else {
                        return answer(stream, self.harden(text_response(StatusCode::FORBIDDEN))).await;
                    };
                    (request, Some(lease))
                }
                Err(rejected) => {
                    let mut response = self.auth_refusal(rejected.request(), rejected.reason(), connection);
                    response.headers_mut().remove(header::CONNECTION);
                    return answer(stream, response).await;
                }
            }
        } else {
            (request, None)
        };
        let route = graphite_meter_core::route::lookup(request.uri().path()).filter(|&route| mounts(listener, route));
        let refusal = self
            .refuse_route(&request, route, lease.as_ref(), peer)
            .or_else(|| route.is_none().then(|| text_response(StatusCode::NOT_FOUND)));
        if let Some(response) = refusal {
            return answer(stream, self.harden(response)).await;
        }
        let route = route.expect("a mounted WebTransport route");
        let class = crate::route::spec(route).admission.expect("WT admission class");
        let route = match route {
            Route::WtPing => SessionRoute::Ping,
            Route::WtDownload => SessionRoute::Download,
            _ => SessionRoute::Upload,
        };
        let owner = lease
            .as_ref()
            .map(AuthLease::owner)
            .unwrap_or_else(|| self.upload_owner(&request, peer));
        let _permit = match self.admission.acquire_keys(class, owner.client_keys()) {
            Ok(permit) => permit,
            Err(error) => {
                let mut response = text_response(StatusCode::from_u16(error.status()).expect("known status"));
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, http::HeaderValue::from_static("1"));
                return answer(stream, self.harden(response)).await;
            }
        };
        let _admitted = credit.work().admit();
        let lifetime = if class == Class::Session {
            self.config.max_session_duration
        } else {
            self.config.max_operation_duration
        };
        let deadline = Instant::now() + lifetime;
        let session = tokio::select! {
            biased;
            _ = lease_ended(lease.clone()) => return Ok(()),
            session = Session::accept(stream) => session?,
        };
        let query = request.uri().query().unwrap_or("");
        let params: Vec<_> = form_urlencoded::parse(query.as_bytes()).collect();
        let value = |name: &str| {
            params
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_ref())
        };
        let datagrams = value("datagrams").is_some_and(datagram_mode);
        let count = download_bytes(query);
        let verify = route == SessionRoute::Download && count == 0;
        let activity = Arc::new(Mutex::new(Instant::now()));
        let mut lanes: FuturesUnordered<Lane> = FuturesUnordered::new();
        let mut controls: FuturesUnordered<Lane> = FuturesUnordered::new();
        let upload_id = value("id").unwrap_or("").to_owned();
        let mut datagram_lane = None;
        let mut refused = false;
        let mut awaiting_credit = false;
        if route == SessionRoute::Download && !verify {
            if datagrams {
                let (session, block, meter) = (&session, self.download_block.clone(), &self.download_meter);
                lanes.push(Box::pin(async move {
                    let transfer = meter.open();
                    let mut since_yield = 0;
                    loop {
                        let mut remaining = count;
                        while remaining > 0 {
                            let size = remaining.min(1000).min(block.len() as u64) as usize;
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
                }));
            } else {
                let count_lanes = value("streams")
                    .and_then(|v| v.parse::<i64>().ok())
                    .filter(|n| *n > 0)
                    .unwrap_or(1)
                    .min(MAX_LANES as i64);
                for _ in 0..count_lanes {
                    lanes.push(Box::pin(download_lane(
                        &session,
                        count,
                        self.download_block.clone(),
                        activity.clone(),
                        &self.download_meter,
                    )));
                }
            }
        } else if route == SessionRoute::Upload {
            let subscription = self.uploads.subscribe(&upload_id, &owner);
            refused = subscription.is_err();
            awaiting_credit = !refused && !credit.fund();
            controls.push(Box::pin(progress(&session, subscription)));
            if datagrams {
                datagram_lane = self.uploads.begin(&upload_id, &owner).ok();
            }
        }
        let datagram_finished: Pin<Box<dyn Future<Output = ()> + Send>> = match &datagram_lane {
            Some(lane) => Box::pin(lane.finished()),
            None => Box::pin(std::future::pending()),
        };
        tokio::pin!(datagram_finished);
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut settle = verify.then(|| Instant::now() + Duration::from_secs(5));
        let mut ending = LaneEnding::Finished;
        loop {
            tokio::select! {
                biased;
                // The peer ended the session, or the connection's shutdown did.
                _ = session.closed() => break,
                _ = lease_ended(lease.clone()) => { ending = LaneEnding::Revoked; break; },
                _ = tick.tick() => {
                    if awaiting_credit {
                        awaiting_credit = !credit.fund();
                    }
                    let last = *activity.lock().expect("WT activity poisoned");
                    if Instant::now().duration_since(last) >= IDLE {
                        ending = LaneEnding::Idle;
                        break;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => { ending = LaneEnding::Lifetime; break; },
                _ = tokio::time::sleep_until(settle.unwrap_or(deadline)), if settle.is_some() => break,
                _ = &mut datagram_finished, if datagram_lane.is_some() => { datagram_lane = None; }
                Some(_) = lanes.next(), if !lanes.is_empty() => {
                    // A download ends with its last lane; later client
                    // streams may replace finished upload lanes.
                    if route == SessionRoute::Download && lanes.is_empty() {
                        break;
                    }
                }
                Some(_) = controls.next(), if !controls.is_empty() => {
                    if refused && settle.is_none() {
                        settle = Some(Instant::now() + REFUSAL_LINGER);
                    }
                }
                payload = session.read_datagram() => {
                    let Some(payload) = payload else { break };
                    match route {
                        SessionRoute::Ping => {
                            touch(&activity);
                            if let Some(reply) = crate::ping::reply(&payload) {
                                let _ = session.send_datagram(reply.as_bytes());
                            }
                        }
                        SessionRoute::Download if datagrams => touch(&activity),
                        SessionRoute::Upload => {
                            if let Some(lane) = &mut datagram_lane {
                                lane.record(payload.len());
                                touch(&activity);
                            }
                        }
                        _ => {} // A route without a datagram lane gets no idle credit.
                    }
                }
                incoming = session.accept_uni() => {
                    // A dropped stream is refused as a cancelled lane.
                    let Some(incoming) = incoming else { break };
                    if route != SessionRoute::Upload || lanes.len() >= MAX_LANES {
                        continue;
                    }
                    match self.uploads.begin(&upload_id, &owner) {
                        Ok(lane) => lanes.push(Box::pin(upload_lane(incoming, lane, activity.clone()))),
                        Err(error) if controls.len() < 2 => controls.push(Box::pin(progress(&session, Err(error)))),
                        Err(_) => {}
                    }
                }
            }
        }
        drop((lanes, controls, datagram_lane));
        session.close(ending.webtransport_code(), ending.reason()).await;
        Ok(())
    }
}

async fn answer(stream: RequestStream, response: Response<ResponseBody>) -> Result<(), http3::Error> {
    let (mut send, _receive) = stream.split();
    let (parts, mut body) = response.into_parts();
    tokio::time::timeout(Duration::from_secs(10), async {
        send.send_response(Response::from_parts(parts, ())).await?;
        while let Some(Ok(frame)) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
            if let Ok(data) = frame.into_data() {
                send.send_data(data).await?;
            }
        }
        send.finish().await
    })
    .await
    .map_err(|_| http3::Error::TimedOut)?
}

fn datagram_mode(value: &str) -> bool {
    let value = value.trim();
    if let Ok(number) = value.parse::<i64>() {
        return number != 0;
    }
    !matches!(value.to_ascii_lowercase().as_str(), "false" | "off" | "no")
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
            let size = remaining.min(16 * 1024).min(block.len() as u64) as usize;
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

async fn upload_lane(
    mut stream: RecvStream,
    mut lane: crate::upload::UploadLane,
    activity: Activity,
) -> Result<(), Failure> {
    while let Ok(Ok(Some(chunk))) = tokio::time::timeout(Duration::from_secs(30), stream.read_chunk()).await {
        lane.record(chunk.len());
        touch(&activity);
    }
    Ok(())
}

async fn progress(
    session: &Session,
    subscription: Result<crate::upload::UploadSubscription, crate::upload::UploadError>,
) -> Result<(), Failure> {
    let mut stream = session.open_uni().await?;
    let mut subscription = match subscription {
        Ok(subscription) => subscription,
        Err(error) => {
            let event = UploadProgress::Error {
                message: error.to_string(),
                code: error.code().to_owned(),
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
            _ = tokio::time::sleep(Duration::from_secs(1)) => "\n".to_owned(),
        };
        stream.write_all(message.as_bytes()).await?;
    }
    Ok(stream.finish()?)
}

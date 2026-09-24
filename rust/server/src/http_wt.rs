//! A CONNECT task owns every application lane; dropping it cancels all session IO.
//! The connection advertises no INITIAL_MAX_* settings, so session flow control
//! is not negotiated. QUIC flow control and the local lane limit remain active.
use super::http_quic::Sessions;
use super::*;
use crate::{
    webtransport::{ReceiveStream, TransportError},
    webtransport_send::ResetQueue,
};
use bytes::Buf;
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_core::{
    capsule::{self, Capsule},
    wire::{self, UploadProgress},
};
use tokio::{io::AsyncReadExt, time::Instant};

pub(super) type H3RequestStream = h3::server::RequestStream<h3_noq::BidiStream<Bytes>, Bytes>;
pub(super) enum SessionEvent {
    Datagram {
        payload: Bytes,
        _budget: tokio::sync::OwnedSemaphorePermit,
    },
    Stream(ReceiveStream),
}
const MAX_LANES: usize = 16;
// A ready datagram send need not yield. Bound each burst so sibling sessions
// still get executor time without paying a scheduler round-trip per packet.
const DATAGRAM_YIELD_BATCH: usize = 16;
const IDLE: Duration = Duration::from_secs(30);
const MAX_CONNECT_DATA: u64 = 1024 * 1024;
const MAX_CONNECT_FRAMES: u64 = 1024;
const RESET: u64 = 0x52e4a40fa8db;
type Lane = Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send>>;
type Activity = Arc<Mutex<Instant>>;
fn touch(activity: &Activity) {
    *activity.lock().expect("WT activity poisoned") = Instant::now();
}

impl HttpServer {
    pub(super) async fn serve_webtransport(
        self: Arc<Self>,
        request: Request<()>,
        mut stream: H3RequestStream,
        quic: quinn::Connection,
        peer: SocketAddr,
        resets: ResetQueue,
        sessions: Sessions,
    ) -> Result<(), TransportError> {
        if request.method() != Method::CONNECT
            || request.extensions().get::<h3::ext::Protocol>()
                != Some(&h3::ext::Protocol::WEB_TRANSPORT)
        {
            return refuse(&mut stream, StatusCode::BAD_REQUEST).await;
        }
        if let Some(response) = self.validate_request(&request) {
            return refuse(&mut stream, response.status()).await;
        }
        let connection = Connection {
            peer,
            tls: true,
            listener: Listener {
                ui: false,
                webtransport: true,
            },
        };
        let (request, lease) = if let Some(auth) = &self.auth {
            match auth.policy().authorize(request, connection) {
                Ok(guard) => {
                    let (request, authorization, _, _) = guard.into_parts();
                    let Authorization::Authenticated(lease) = authorization else {
                        return refuse(&mut stream, StatusCode::FORBIDDEN).await;
                    };
                    (request, Some(lease))
                }
                Err(rejected) => {
                    let response =
                        self.auth_refusal(rejected.request(), rejected.reason(), connection);
                    let mut response = response.map(|_| ());
                    response.headers_mut().remove(header::CONNECTION);
                    return tokio::time::timeout(Duration::from_secs(10), async {
                        stream.send_response(response).await?;
                        stream.finish().await?;
                        Ok::<_, TransportError>(())
                    })
                    .await?;
                }
            }
        } else {
            (request, None)
        };
        let path = request.uri().path();
        if !matches!(path, "/wt/ping" | "/wt/download" | "/wt/upload") {
            return refuse(&mut stream, StatusCode::NOT_FOUND).await;
        }
        let owner = lease
            .as_ref()
            .map(AuthLease::owner)
            .unwrap_or_else(|| self.upload_owner(&request, peer));
        let class = if path == "/wt/ping" {
            Class::Request
        } else {
            Class::Session
        };
        let key = if class == Class::Session {
            lease
                .as_ref()
                .map(|lease| format!("login:{}", lease.session().id()))
                .unwrap_or_else(|| owner.budget_key().to_owned())
        } else {
            owner.budget_key().to_owned()
        };
        let _permit = match self.admission.acquire(class, &key) {
            Ok(permit) => permit,
            Err(error) => return refuse(&mut stream, StatusCode::from_u16(error.status())?).await,
        };
        let lifetime = if class == Class::Session {
            self.config.max_session_duration
        } else {
            self.config.max_operation_duration
        };
        let deadline = Instant::now() + lifetime;
        let session_id = stream.send_id().into_inner();
        let Some((_registration, mut events)) = sessions.register(session_id) else {
            stream.stop_sending(h3::error::Code::H3_REQUEST_REJECTED);
            stream.stop_stream(h3::error::Code::H3_REQUEST_REJECTED);
            return Ok(());
        };
        tokio::select! { biased; _ = lease_ended(lease.clone()) => return Ok(()), result = tokio::time::timeout(Duration::from_secs(10), stream.send_response(Response::builder().status(200).body(())?)) => result?? }
        let query = request.uri().query().unwrap_or("");
        let params: Vec<_> = url::form_urlencoded::parse(query.as_bytes()).collect();
        let value = |name: &str| {
            params
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_ref())
        };
        let datagrams = value("datagrams").is_some_and(datagram_mode);
        let count = download_bytes(query);
        let verify = path == "/wt/download" && count == 0;
        let activity = Arc::new(Mutex::new(Instant::now()));
        let mut lanes: FuturesUnordered<Lane> = FuturesUnordered::new();
        let mut controls: FuturesUnordered<Lane> = FuturesUnordered::new();
        let upload_id = value("id").unwrap_or("").to_owned();
        let mut datagram_lane = None;
        if path == "/wt/download" && !verify {
            if datagrams {
                let quic = quic.clone();
                let block = self.download_block.clone();
                lanes.push(Box::pin(async move {
                    let mut since_yield = 0;
                    loop {
                        let mut remaining = count;
                        while remaining > 0 {
                            let size = remaining.min(1000).min(block.len() as u64) as usize;
                            quic.send_datagram_wait(frame_datagram(session_id, &block[..size])?)
                                .await?;
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
                        quic.clone(),
                        resets.clone(),
                        session_id,
                        count,
                        self.download_block.clone(),
                        activity.clone(),
                    )));
                }
            }
        } else if path == "/wt/upload" {
            let subscription = self.uploads.subscribe(&upload_id, &owner);
            controls.push(Box::pin(progress(
                quic.clone(),
                resets.clone(),
                session_id,
                subscription,
            )));
            if datagrams {
                datagram_lane = self.uploads.begin(&upload_id, &owner).ok();
            }
        }
        let datagram_finished: Pin<Box<dyn Future<Output = ()> + Send>> = match &datagram_lane {
            Some(lane) => Box::pin(lane.finished()),
            None => Box::pin(std::future::pending()),
        };
        tokio::pin!(datagram_finished);
        let mut decoder = capsule::Decoder::new();
        let mut connect_bytes = 0_u64;
        let mut connect_frames = 0_u64;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let verify_deadline = Instant::now() + IDLE;
        let result: Result<(), TransportError> = async {
            loop {
                // A peer may keep CONNECT DATA continuously ready. Let Tokio
                // rotate ready work so that it cannot starve session streams
                // and datagrams. Recheck authorization before each operation;
                // the revocation future below wakes a blocked loop promptly.
                if lease.as_ref().is_some_and(|lease| !lease.is_active())
                    || Instant::now() >= deadline
                {
                    break;
                }
                tokio::select! {
                    _ = lease_ended(lease.clone()) => break,
                    _ = tokio::time::sleep_until(deadline) => break,
                    _ = tick.tick() => {
                        if verify && Instant::now() >= verify_deadline { break; }
                        if !verify && activity.lock().expect("WT activity poisoned").elapsed() >= IDLE { break; }

                    }
                    _ = &mut datagram_finished, if datagram_lane.is_some() => { datagram_lane = None; }
                    data = stream.recv_data() => {
                        let Some(mut data) = data? else { decoder.finish()?; break; };
                        connect_bytes = connect_bytes.saturating_add(data.remaining() as u64);
                        connect_frames += 1;
                        if connect_bytes > MAX_CONNECT_DATA || connect_frames > MAX_CONNECT_FRAMES {
                            return Err("WebTransport CONNECT control-data budget exceeded".into());
                        }
                        let data = data.copy_to_bytes(data.remaining());
                        if decoder.feed(&data)?.iter().any(|capsule| matches!(capsule, Capsule::CloseSession { .. })) { break; }
                    }
                    Some(_) = lanes.next(), if !lanes.is_empty() => {
                        // Download lanes run until their stream can no longer
                        // make progress. Once every lane has ended, the
                        // CONNECT has no remaining payload producer.
                        // Upload lanes may finish normally and be replaced by
                        // later client-opened streams.
                        if path == "/wt/download" && lanes.is_empty() {
                            break;
                        }
                    }
                    Some(_) = controls.next(), if !controls.is_empty() => {}
                    event = events.recv() => match event {
                        None => break,
                        Some(SessionEvent::Datagram { payload, _budget }) => {
                            match path {
                                "/wt/ping" => {
                                    touch(&activity);
                                    if let Some(reply) = crate::ping::reply(&payload) {
                                        let _ = quic.send_datagram(frame_datagram(session_id, reply.as_bytes())?);
                                    }
                                }
                                "/wt/download" if datagrams => touch(&activity),
                                "/wt/upload" => {
                                    if let Some(lane) = &mut datagram_lane {
                                        lane.record(payload.len());
                                        touch(&activity);
                                    }
                                }
                                _ => {} // A route without a datagram lane gets no idle credit.
                            }
                        }
                        Some(SessionEvent::Stream(mut incoming)) => {
                            if path != "/wt/upload" || lanes.len() >= MAX_LANES { h3::quic::RecvStream::stop_sending(&mut incoming, RESET); continue; }
                            match self.uploads.begin(&upload_id, &owner) {
                                Ok(lane) => lanes.push(Box::pin(upload_lane(incoming, lane, activity.clone()))),
                                Err(error) => {
                                    h3::quic::RecvStream::stop_sending(&mut incoming, RESET);
                                    if controls.len() < 2 { controls.push(Box::pin(progress(quic.clone(), resets.clone(), session_id, Err(error)))); }
                                }
                            }
                        }
                    }
                }
            }
            Ok(())
        }.await;
        drop(lanes);
        drop(controls);
        drop(datagram_lane);
        // Close is bounded even when the peer stops reading the CONNECT stream.
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            stream
                .send_data(Bytes::from(capsule::encode_close(0, "")))
                .await?;
            stream.finish().await?;
            Ok::<_, TransportError>(())
        })
        .await;
        result
    }
}

async fn refuse(stream: &mut H3RequestStream, status: StatusCode) -> Result<(), TransportError> {
    let mut response = Response::builder().status(status);
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        response = response.header(header::RETRY_AFTER, "1");
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        stream.send_response(response.body(())?).await?;
        stream.finish().await?;
        Ok::<_, TransportError>(())
    })
    .await?
}
fn datagram_mode(value: &str) -> bool {
    let value = value.trim();
    if let Ok(number) = value.parse::<i64>() {
        return number != 0;
    }
    !matches!(value.to_ascii_lowercase().as_str(), "false" | "off" | "no")
}
fn frame_datagram(id: u64, payload: &[u8]) -> Result<Bytes, TransportError> {
    let mut frame = Vec::with_capacity(8 + payload.len());
    capsule::encode_varint(id / 4, &mut frame)?;
    frame.extend_from_slice(payload);
    Ok(frame.into())
}
async fn download_lane(
    quic: quinn::Connection,
    resets: ResetQueue,
    id: u64,
    count: u64,
    block: Bytes,
    activity: Activity,
) -> Result<(), TransportError> {
    loop {
        let mut stream = resets
            .open(&quic, id, quinn::VarInt::from_u64(RESET)?)
            .await?;
        let mut remaining = count;
        while remaining > 0 {
            let size = remaining.min(block.len() as u64) as usize;
            // Noq retains queued stream data for retransmission. Reuse the
            // shared immutable block instead of copying every write into its
            // send buffer; `write_chunk` handles partial flow-control writes.
            match stream.write_chunk(block.slice(..size)).await {
                Ok(()) => {}
                Err(error) if remaining == count => return Err(error.into()),
                Err(_) => break,
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
    stream: ReceiveStream,
    mut lane: crate::upload::UploadLane,
    activity: Activity,
) -> Result<(), TransportError> {
    let mut stream = IncomingLane(stream);
    let mut block = vec![0; 64 * 1024];
    loop {
        match tokio::time::timeout(Duration::from_secs(120), stream.0.read(&mut block)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(count)) => {
                lane.record(count);
                touch(&activity);
            }
        }
    }
    Ok(())
}
struct IncomingLane(ReceiveStream);
impl Drop for IncomingLane {
    fn drop(&mut self) {
        h3::quic::RecvStream::stop_sending(&mut self.0, RESET);
    }
}
async fn progress(
    quic: quinn::Connection,
    resets: ResetQueue,
    id: u64,
    subscription: Result<crate::upload::UploadSubscription, crate::upload::UploadError>,
) -> Result<(), TransportError> {
    let mut stream = resets
        .open(&quic, id, quinn::VarInt::from_u64(RESET)?)
        .await?;
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
            stream.finish()?;
            return Ok(());
        }
    };
    loop {
        let message = tokio::select! {
            event = subscription.next() => { let Some(event) = event else { break; }; format!("{}\n", wire::encode_upload_progress(&event)?) },
            _ = tokio::time::sleep(Duration::from_secs(1)) => "\n".to_owned(),
        };
        stream.write_all(message.as_bytes()).await?;
    }
    stream.finish()?;
    Ok(())
}

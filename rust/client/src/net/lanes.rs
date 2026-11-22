//! Lane groups: which connection or session a stage's lanes share, and the one loop every lane runs.
use super::{
    Attempt, CONTROL_TIMEOUT, Client, Incoming, Request, Retry,
    conn::Session,
    conn::{Conn, Failed, Payload, ReadBuffer, TRANSFER_BYTES},
    fault::Fault,
    lock,
};
use crate::model::{Dir, Direction, Failure, LaneHealth, Stage};
use bytes::Bytes;
use graphite_meter_proto::{
    discovery::{Protocol, ThroughputTransport},
    lane::MAX_LANE_STREAMS,
    origin::Origin,
    refusal::Refusal,
    route::Route,
};
use http::Method;
use std::{
    future::Future,
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{runtime::Handle, sync::watch, task::JoinSet, time::timeout};
use tokio_util::sync::CancellationToken;

const STREAM_BYTES: u64 = 64 << 20;
/// The block a direct upload repeats; 256 KiB cuts syscall overhead on fast links.
const BLOCK_BYTES: usize = 256 << 10;
/// An upload attempt that handed on more than this counts as having moved bytes when deciding retries.
const MOVED_BYTES: u64 = 64 << 10;
/// A WebTransport download stream's chunks taken under one lock of the connection, counted at once.
const CHUNKS_PER_READ: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThroughputPath {
    pub origin: Origin,
    pub transport: ThroughputTransport,
    pub protocol: Protocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    /// A connection the group dials: HTTP/1.1 for one lane, HTTP/2 or HTTP/3 for many.
    Dialed,
    /// The path's HTTP/3 connection, which control requests use too.
    Path,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPlan {
    pub path: ThroughputPath,
    pub direction: Direction,
    pub carrier: Carrier,
    pub lanes: Range<usize>,
}

/// Groups: HTTP/1.1 per lane, HTTP/2 per direction, HTTP/3 downloads the path's and uploads a fresh one (free of the download's congestion state), WebTransport up to its lane cap each.
pub fn topology(path: &ThroughputPath, stage: Stage, lanes: Dir<usize>) -> Vec<GroupPlan> {
    let mut plans = Vec::new();
    for &direction in stage.directions() {
        let (carrier, size) = match (path.transport, path.protocol) {
            (ThroughputTransport::FetchStream, Protocol::Http3) if direction == Direction::Down => {
                (Carrier::Path, usize::MAX)
            }
            (ThroughputTransport::FetchStream, Protocol::Http2 | Protocol::Http3) => {
                (Carrier::Dialed, usize::MAX)
            }
            (ThroughputTransport::FetchStream, _) => (Carrier::Dialed, 1),
            _ => (Carrier::Session, MAX_LANE_STREAMS),
        };
        let count = lanes[direction];
        plans.extend((0..count).step_by(size).map(|first| GroupPlan {
            path: path.clone(),
            direction,
            carrier,
            lanes: first..count.min(first.saturating_add(size)),
        }));
    }
    plans
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Work {
    Download,
    Upload(String),
}

pub struct Lanes {
    tally: Arc<Tally>,
    tasks: JoinSet<Option<Fault>>,
    count: usize,
    failed: Option<Fault>,
}

#[derive(Default)]
struct Tally {
    bytes: AtomicU64,
    ready: AtomicUsize,
    /// Each lane's failure while it retries without moving bytes.
    retrying: Mutex<Vec<Option<Failure>>>,
    session: watch::Sender<Option<Arc<Session>>>,
}

impl Tally {
    fn retrying(&self, lane: usize, failure: Option<Failure>) {
        if let Some(slot) = lock(&self.retrying).get_mut(lane) {
            *slot = failure;
        }
    }
}

impl Lanes {
    /// Starts `work` on its direction's plans, lane `n` after `n` staggers, a group per runtime, until `token` cancels.
    pub fn start(
        client: &Client,
        plans: Vec<GroupPlan>,
        work: Work,
        stagger: Duration,
        token: CancellationToken,
    ) -> Self {
        let direction = match work {
            Work::Download => Direction::Down,
            Work::Upload(_) => Direction::Up,
        };
        let plans: Vec<_> = plans
            .into_iter()
            .filter(|plan| plan.direction == direction)
            .collect();
        let count = plans.iter().map(|plan| plan.lanes.len()).sum();
        let tally = Arc::new(Tally {
            retrying: Mutex::new(vec![None; count]),
            ..Tally::default()
        });
        let block = if direction == Direction::Up {
            block()
        } else {
            Bytes::new()
        };
        let mut tasks = JoinSet::new();
        for plan in plans {
            let path = (plan.carrier == Carrier::Path).then_some(&plan.path);
            let shared = path.and_then(|path| client.connections.home(&path.origin, path.protocol));
            let home = shared.unwrap_or_else(|| client.shared.runtimes.next());
            let (client, work, block) = (client.clone(), work.clone(), block.clone());
            let group = Group {
                client,
                plan,
                work,
                block,
                conn: Slot(Default::default()),
                session: Slot(Default::default()),
                direct_down_disabled: AtomicBool::new(false),
            };
            let group = Arc::new(group);
            for lane in group.plan.lanes.clone() {
                let (group, tally, token) = (group.clone(), tally.clone(), token.clone());
                let delay = stagger.saturating_mul(u32::try_from(lane).unwrap_or(u32::MAX));
                tasks.spawn_on(
                    async move {
                        token
                            .run_until_cancelled(group.run(lane, tally, delay))
                            .await
                    },
                    &home,
                );
            }
        }
        Self {
            tally,
            tasks,
            count,
            failed: None,
        }
    }

    /// Payload bytes the download lanes consumed; upload lanes count none, as the receiver counts uploads.
    pub fn bytes(&self) -> u64 {
        self.tally.bytes.load(Ordering::Relaxed)
    }

    /// Every lane opened once: a download its first answer, an upload its first request.
    pub fn ready(&self) -> bool {
        self.tally.ready.load(Ordering::Relaxed) >= self.count
    }

    /// Failed once any lane's fault stood, else retrying while a lane fails without moving bytes.
    pub fn health(&mut self) -> LaneHealth {
        while let Some(ended) = self.tasks.try_join_next() {
            let fault = match ended {
                Ok(Some(fault)) => fault,
                Ok(None) => continue,
                Err(error) => error.into(),
            };
            self.failed.get_or_insert(fault);
        }
        if let Some(fault) = &self.failed {
            return LaneHealth::Failed(fault.failure());
        }
        match lock(&self.tally.retrying).iter().flatten().next() {
            Some(failure) => LaneHealth::Retrying(failure.clone()),
            None => LaneHealth::Ok,
        }
    }

    pub fn refusal(&self) -> Option<Refusal> {
        if let Some(Fault::Refused(refusal)) = self.failed {
            Some(refusal)
        } else {
            None
        }
    }

    /// The latest session WebTransport upload lanes opened, whose first server stream carries the receiver's feed.
    pub fn session(&self) -> watch::Receiver<Option<Arc<Session>>> {
        self.tally.session.subscribe()
    }
}

/// A random block, so no hop compresses the upload; a host without randomness cannot measure and panics.
fn block() -> Bytes {
    Bytes::copy_from_slice(&graphite_meter_net::random::<BLOCK_BYTES>())
}

/// A group's shared connection or session, replaced one dial at a time once unusable.
struct Slot<T>(tokio::sync::Mutex<Option<T>>);

impl<T> Slot<T> {
    /// A `handle` to the one `usable` allows, else to what `dial` brings; without one, as HTTP/1.1, the lane keeps it.
    async fn get(
        &self,
        dial: impl Future<Output = Result<T, Fault>>,
        usable: fn(&T) -> bool,
        handle: fn(&T) -> Option<T>,
    ) -> Result<T, Fault> {
        let mut current = self.0.lock().await;
        if let Some(handle) = current
            .as_ref()
            .filter(|shared| usable(shared))
            .and_then(handle)
        {
            return Ok(handle);
        }
        let dialed = dial.await?;
        let Some(handle) = handle(&dialed) else {
            return Ok(dialed);
        };
        *current = Some(dialed);
        Ok(handle)
    }
}

struct Group {
    client: Client,
    plan: GroupPlan,
    work: Work,
    block: Bytes,
    conn: Slot<Conn>,
    session: Slot<Arc<Session>>,
    direct_down_disabled: AtomicBool,
}

struct Lane {
    number: usize,
    announced: bool,
    moved: bool,
    sent: Arc<AtomicU64>,
}

impl Lane {
    fn announce(&mut self, tally: &Tally) {
        if !std::mem::replace(&mut self.announced, true) {
            tally.ready.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Group {
    async fn run(self: Arc<Self>, number: usize, tally: Arc<Tally>, delay: Duration) -> Fault {
        tokio::time::sleep(delay).await;
        let mut lane = Lane {
            number,
            announced: false,
            moved: false,
            sent: Arc::default(),
        };
        let mut retry = Retry::default();
        loop {
            let (started, sent) = (Instant::now(), lane.sent.load(Ordering::Relaxed));
            lane.moved = false;
            let result = self.attempt(&mut lane, &tally).await;
            let moved = lane.moved || lane.sent.load(Ordering::Relaxed) - sent > MOVED_BYTES;
            let fault = match result {
                Ok(()) if moved => {
                    retry = Retry::default();
                    tally.retrying(number, None);
                    continue;
                }
                Ok(()) => Fault::TimedOut("transfer"),
                Err(fault) => fault,
            };
            let moved = moved
                && !matches!(
                    fault,
                    Fault::Status { .. } | Fault::SignIn(_) | Fault::Refused(_)
                );
            tally.retrying(number, (!moved).then(|| fault.failure()));
            match retry.after(fault, Attempt { started, moved }, Instant::now()) {
                Ok(pause) => tokio::time::sleep(pause).await,
                Err(fault) => return fault,
            }
        }
    }

    async fn attempt(&self, lane: &mut Lane, tally: &Tally) -> Result<(), Fault> {
        let (
            ThroughputPath {
                origin, protocol, ..
            },
            client,
        ) = (&self.plan.path, &self.client);
        if let Carrier::Session = self.plan.carrier {
            let (dial, usable) = (self.dial_session(tally), |session: &Arc<Session>| {
                session.usable()
            });
            let session = self
                .session
                .get(dial, usable, |session| Some(session.clone()))
                .await?;
            return match &self.work {
                Work::Download => receive(&session, lane, tally).await,
                Work::Upload(_) => send(&session, &self.block, lane, tally).await,
            };
        }
        let conn = || async {
            match self.plan.carrier {
                Carrier::Dialed => {
                    let down = self.plan.direction == Direction::Down;
                    let buffer = if down {
                        ReadBuffer::Fixed
                    } else {
                        ReadBuffer::Adaptive
                    };
                    let dial = client.dial(origin, *protocol, buffer);
                    self.conn.get(dial, Conn::usable, Conn::share).await
                }
                _ => client.connections.shared(client, origin, *protocol).await,
            }
        };
        match &self.work {
            Work::Download => {
                let query = vec![
                    ("bytes", TRANSFER_BYTES.to_string()),
                    ("cb", cache_buster()),
                    ("lane", lane.number.to_string()),
                ];
                let request = self.request(Method::GET, Route::Download, query);
                #[cfg(target_os = "linux")]
                if self.plan.carrier == Carrier::Dialed
                    && *protocol == Protocol::Http1
                    && !self.direct_down_disabled.load(Ordering::Relaxed)
                    && let Some(socket) = client.shared.connector.connect_plain(origin).await?
                    && direct_download(
                        socket,
                        client,
                        &request,
                        lane,
                        tally,
                        &self.direct_down_disabled,
                    )
                    .await?
                {
                    return Ok(());
                }
                let opened = open(conn, client, &request, Payload::empty);
                let opened = timeout(CONTROL_TIMEOUT, opened).await;
                let (_conn, body) = opened.unwrap_or(Err(Fault::TimedOut("download answer")))?;
                self.download(body, lane, tally).await
            }
            Work::Upload(id) => {
                if self.plan.carrier == Carrier::Dialed
                    && *protocol == Protocol::Http1
                    && let Some(socket) = client.shared.connector.connect_plain(origin).await?
                {
                    let query = vec![
                        ("cb", cache_buster()),
                        ("id", id.to_owned()),
                        ("lane", lane.number.to_string()),
                    ];
                    let request = self.request(Method::POST, Route::Upload, query);
                    return direct_upload(socket, client, &request, &self.block, lane, tally).await;
                }
                self.upload(conn, id, lane, tally).await
            }
        }
    }

    /// The group's session, dialed on the lane's runtime; an upload's is announced for its feed.
    async fn dial_session(&self, tally: &Tally) -> Result<Arc<Session>, Fault> {
        let (route, query) = match &self.work {
            Work::Download => {
                let streams = self.plan.lanes.len().to_string();
                (
                    Route::WtDownload,
                    vec![("bytes", STREAM_BYTES.to_string()), ("streams", streams)],
                )
            }
            Work::Upload(id) => (Route::WtUpload, vec![("id", id.clone())]),
        };
        let home = Handle::current();
        let session = self
            .client
            .session(Some(&home), &self.plan.path.origin, route, query);
        let session = Arc::new(session.await?);
        if let Work::Upload(_) = self.work {
            tally.session.send_replace(Some(session.clone()));
        }
        Ok(session)
    }

    fn request(&self, method: Method, route: Route, query: Vec<(&'static str, String)>) -> Request {
        Request {
            query,
            ..Request::new(method, &self.plan.path.origin, route)
        }
    }

    async fn download(
        &self,
        mut body: Incoming,
        lane: &mut Lane,
        tally: &Tally,
    ) -> Result<(), Fault> {
        lane.announce(tally);
        while let Some(chunk) = body.chunk().await? {
            tally.bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            lane.moved |= !chunk.is_empty();
        }
        Ok(())
    }

    async fn upload<F: Future<Output = Result<Conn, Fault>>>(
        &self,
        conn: impl Fn() -> F,
        id: &str,
        lane: &mut Lane,
        tally: &Tally,
    ) -> Result<(), Fault> {
        let query = vec![
            ("cb", cache_buster()),
            ("id", id.to_owned()),
            ("lane", lane.number.to_string()),
        ];
        let request = self.request(Method::POST, Route::Upload, query);
        lane.announce(tally);
        let block = self.block.slice(..(64 << 10));
        let payload = || Payload::repeat(block.clone(), lane.sent.clone());
        let (_conn, mut answer) = open(conn, &self.client, &request, payload).await?;
        while answer.chunk().await?.is_some() {}
        Ok(())
    }
}

async fn open<F: Future<Output = Result<Conn, Fault>>>(
    conn: impl Fn() -> F,
    client: &Client,
    request: &Request,
    payload: impl Fn() -> Payload,
) -> Result<(Conn, Incoming), Fault> {
    let mut first = conn().await?;
    match first.open(client, request, payload()).await {
        Err(Failed { unsent: true, .. }) => {
            let mut fresh = conn().await?;
            let answer = fresh.open(client, request, payload()).await;
            Ok((fresh, answer.map_err(|failed| failed.fault)?))
        }
        answer => Ok((first, answer.map_err(|failed| failed.fault)?)),
    }
}

/// One server stream of the session, counted as it arrives; the lane opens at its first bytes.
async fn receive(session: &Session, lane: &mut Lane, tally: &Tally) -> Result<(), Fault> {
    let mut stream = session.accept_uni().await?;
    let (mut chunks, mut received) = ([const { Bytes::new() }; CHUNKS_PER_READ], 0);
    while let Some(count) = stream
        .read_chunks(&mut chunks)
        .await
        .map_err(|error| session.fault(error))?
    {
        let taken = chunks.iter_mut().take(count).map(std::mem::take);
        let size = taken.map(|chunk| chunk.len() as u64).sum::<u64>();
        received += size;
        tally.bytes.fetch_add(size, Ordering::Relaxed);
        if size > 0 {
            lane.moved = true;
            lane.announce(tally);
        }
        if received > STREAM_BYTES {
            return Err(Fault::Malformed(
                "WebTransport download exceeded its byte count".into(),
            ));
        }
    }
    match received == STREAM_BYTES {
        true => Ok(()),
        false => Err(Fault::Lost),
    }
}

async fn send(
    session: &Session,
    block: &Bytes,
    lane: &mut Lane,
    tally: &Tally,
) -> Result<(), Fault> {
    loop {
        let mut stream = session.open_uni().await?;
        lane.announce(tally);
        let mut payload = Payload::repeat(block.clone(), lane.sent.clone());
        while let Some(slice) = payload.slice() {
            stream
                .write_chunk(slice)
                .await
                .map_err(|error| session.fault(error))?;
        }
        stream.finish().map_err(|error| session.fault(error))?;
    }
}

fn cache_buster() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH);
    now.unwrap_or_default().as_nanos().to_string()
}

fn format_h1_request(head: &http::Request<()>, content_length: Option<u64>) -> Vec<u8> {
    use std::io::Write;
    let path = head.uri().path_and_query().map_or("/", |pq| pq.as_str());
    let mut wire = Vec::with_capacity(256);
    let _ = write!(wire, "{} {} HTTP/1.1\r\n", head.method(), path);
    if let Some(authority) = head.uri().authority() {
        let _ = write!(wire, "host: {}\r\n", authority.as_str());
    }
    for (name, value) in head.headers() {
        let _ = write!(wire, "{}: ", name.as_str());
        wire.extend_from_slice(value.as_bytes());
        wire.extend_from_slice(b"\r\n");
    }
    if let Some(len) = content_length {
        let _ = write!(
            wire,
            "content-type: application/octet-stream\r\ncontent-length: {len}\r\n"
        );
    }
    wire.extend_from_slice(b"connection: close\r\n\r\n");
    wire
}

async fn read_h1_response_head<R: tokio::io::AsyncRead + Unpin>(
    socket: &mut R,
) -> Result<(http::StatusCode, http::HeaderMap, Vec<u8>), Fault> {
    use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
    use tokio::io::AsyncReadExt;

    const MAX_HEAD: usize = 16 << 10;
    const MAX_INFO: usize = 16;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let mut consumed_head = 0usize;
    let mut info_count = 0usize;

    loop {
        let split = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            if consumed_head.saturating_add(buf.len()) > MAX_HEAD {
                return Err(Fault::Malformed("response head exceeded 16 KiB".into()));
            }
            let n = socket.read(&mut chunk).await.map_err(|_| Fault::Lost)?;
            if n == 0 {
                return Err(Fault::Lost);
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        if consumed_head.saturating_add(split + 4) > MAX_HEAD {
            return Err(Fault::Malformed("response head exceeded 16 KiB".into()));
        }
        let head = &buf[..split];
        let mut lines = head.split(|&b| b == b'\n');
        let status_raw = lines
            .next()
            .ok_or_else(|| Fault::Malformed("missing HTTP status line".into()))?;
        let status_raw = status_raw.strip_suffix(b"\r").unwrap_or(status_raw);
        let status_line = std::str::from_utf8(status_raw)
            .map_err(|_| Fault::Malformed("status line was not UTF-8".into()))?;
        let rest = status_line
            .strip_prefix("HTTP/1.1 ")
            .or_else(|| status_line.strip_prefix("HTTP/1.0 "))
            .ok_or_else(|| Fault::Malformed("unsupported HTTP status line".into()))?;
        let code = rest
            .split(' ')
            .next()
            .filter(|code| code.len() == 3 && code.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|code| code.parse::<u16>().ok())
            .and_then(|code| StatusCode::from_u16(code).ok())
            .ok_or_else(|| Fault::Malformed("invalid HTTP status code".into()))?;
        let mut headers = HeaderMap::new();
        for raw_line in lines {
            let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
            if line.is_empty() {
                continue;
            }
            let Some(colon) = line.iter().position(|&b| b == b':') else {
                return Err(Fault::Malformed("malformed HTTP response header".into()));
            };
            let name = HeaderName::from_bytes(&line[..colon])
                .map_err(|_| Fault::Malformed("invalid HTTP header name".into()))?;
            let mut val = &line[colon + 1..];
            while let Some(&b) = val.first()
                && matches!(b, b' ' | b'\t')
            {
                val = &val[1..];
            }
            while let Some(&b) = val.last()
                && matches!(b, b' ' | b'\t')
            {
                val = &val[..val.len() - 1];
            }
            let value = HeaderValue::from_bytes(val)
                .map_err(|_| Fault::Malformed("invalid HTTP header value".into()))?;
            headers.append(name, value);
        }
        if code.is_informational() && code != StatusCode::SWITCHING_PROTOCOLS {
            info_count += 1;
            if info_count > MAX_INFO {
                return Err(Fault::Malformed(
                    "too many informational HTTP responses".into(),
                ));
            }
            consumed_head += split + 4;
            buf.drain(..split + 4);
            continue;
        }
        let trailing = buf.split_off(split + 4);
        return Ok((code, headers, trailing));
    }
}

/// Drains a plain HTTP/1.1 `Transfer-Encoding: chunked` body directly so a re-chunking proxy never wastes a GET.
#[cfg(target_os = "linux")]
async fn drain_chunked(
    socket: &mut tokio::net::TcpStream,
    trailing: Vec<u8>,
    lane: &mut Lane,
    tally: &Tally,
) -> Result<(), Fault> {
    use tokio::io::AsyncReadExt;

    const MAX_LINE: usize = 8 << 10;
    const YIELD_BYTES: u64 = 4 << 20;

    let mut buf = trailing;
    let mut read_buf = vec![0u8; 64 << 10];
    let mut since_yield: u64 = 0;

    async fn read_more(
        socket: &mut tokio::net::TcpStream,
        buf: &mut Vec<u8>,
        scratch: &mut [u8],
    ) -> Result<(), Fault> {
        let n = socket.read(scratch).await.map_err(|_| Fault::Lost)?;
        if n == 0 {
            return Err(Fault::Lost);
        }
        buf.extend_from_slice(&scratch[..n]);
        Ok(())
    }

    loop {
        // Read chunk-size line terminated by CRLF.
        let line_end = loop {
            if let Some(pos) = buf.windows(2).position(|w| w == b"\r\n") {
                break pos;
            }
            if buf.len() >= MAX_LINE {
                return Err(Fault::Lost);
            }
            read_more(socket, &mut buf, &mut read_buf).await?;
        };
        let size_line = std::str::from_utf8(&buf[..line_end]).map_err(|_| Fault::Lost)?;
        let hex = size_line.split(';').next().unwrap_or("").trim();
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Fault::Lost);
        }
        let mut remaining = u64::from_str_radix(hex, 16).map_err(|_| Fault::Lost)?;
        buf.drain(..line_end + 2);
        if remaining == 0 {
            // Drain optional trailer section ending with an empty line.
            loop {
                if buf.starts_with(b"\r\n") {
                    return Ok(());
                }
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let _ = pos;
                    return Ok(());
                }
                if buf.len() >= MAX_LINE {
                    return Err(Fault::Lost);
                }
                read_more(socket, &mut buf, &mut read_buf).await?;
            }
        }
        // Consume `remaining` payload bytes, counting only decoded payload bytes.
        let buffered = (buf.len() as u64).min(remaining) as usize;
        if buffered > 0 {
            buf.drain(..buffered);
            tally.bytes.fetch_add(buffered as u64, Ordering::Relaxed);
            lane.moved = true;
            remaining -= buffered as u64;
            since_yield += buffered as u64;
        }
        while remaining > 0 {
            let cap = remaining.min(read_buf.len() as u64) as usize;
            let n = socket
                .read(&mut read_buf[..cap])
                .await
                .map_err(|_| Fault::Lost)?;
            if n == 0 {
                return Err(Fault::Lost);
            }
            tally.bytes.fetch_add(n as u64, Ordering::Relaxed);
            lane.moved = true;
            remaining -= n as u64;
            since_yield += n as u64;
            if since_yield >= YIELD_BYTES {
                since_yield = 0;
                tokio::task::yield_now().await;
            }
        }
        // Consume the trailing CRLF after the chunk payload.
        while buf.len() < 2 {
            read_more(socket, &mut buf, &mut read_buf).await?;
        }
        if &buf[..2] != b"\r\n" {
            return Err(Fault::Lost);
        }
        buf.drain(..2);
    }
}

/// Drains a close-delimited HTTP/1.1 body until EOF so a relay without Content-Length never wastes a GET.
#[cfg(target_os = "linux")]
async fn drain_close_delimited(
    socket: &mut tokio::net::TcpStream,
    trailing: Vec<u8>,
    lane: &mut Lane,
    tally: &Tally,
) -> Result<(), Fault> {
    use rustix::net::{RecvFlags, recv};
    use tokio::io::Interest;

    const YIELD_BYTES: u64 = 4 << 20;

    if !trailing.is_empty() {
        tally
            .bytes
            .fetch_add(trailing.len() as u64, Ordering::Relaxed);
        lane.moved = true;
    }
    let mut discard: Vec<u8> = Vec::with_capacity(256 << 10);
    let mut since_yield: u64 = 0;
    loop {
        let cap = discard.capacity();
        let read = socket.try_io(Interest::READABLE, || {
            let (_, len) = recv(
                &*socket,
                &mut discard.spare_capacity_mut()[..cap],
                RecvFlags::TRUNC,
            )?;
            Ok(len)
        });
        match read {
            Ok(0) => return Ok(()),
            Ok(n) => {
                let n = n.min(cap);
                tally.bytes.fetch_add(n as u64, Ordering::Relaxed);
                lane.moved = true;
                since_yield += n as u64;
                if since_yield >= YIELD_BYTES {
                    since_yield = 0;
                    tokio::task::yield_now().await;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                since_yield = 0;
                socket.readable().await.map_err(|_| Fault::Lost)?;
            }
            Err(_) => return Err(Fault::Lost),
        }
    }
}

#[cfg(target_os = "linux")]
fn parse_content_length_headers(headers: &http::HeaderMap) -> Result<Option<u64>, ()> {
    let mut values = headers.get_all(http::header::CONTENT_LENGTH).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    let mut parsed: Option<u64> = None;
    for header in std::iter::once(first).chain(values) {
        let text = header.to_str().map_err(|_| ())?;
        for part in text.split(',') {
            let trimmed = part.trim_matches(|c| matches!(c, ' ' | '\t'));
            if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
                return Err(());
            }
            let len = trimmed.parse::<u64>().map_err(|_| ())?;
            if let Some(prev) = parsed {
                if prev != len {
                    return Err(());
                }
            } else {
                parsed = Some(len);
            }
        }
    }
    Ok(parsed)
}

/// Zero-copy plain HTTP/1.1 download using `MSG_TRUNC` and adaptive `SO_RCVLOWAT`; returns `Ok(false)` to fall back.
#[cfg(target_os = "linux")]
async fn direct_download(
    mut socket: tokio::net::TcpStream,
    client: &Client,
    request: &Request,
    lane: &mut Lane,
    tally: &Tally,
    disable_direct: &AtomicBool,
) -> Result<bool, Fault> {
    use graphite_meter_net::lowat::{BulkRead, READ_LOWAT_MIN};
    use http::header;
    use rustix::net::{RecvFlags, recv};
    use tokio::io::{AsyncWriteExt, Interest};

    const YIELD_BYTES: u64 = 4 << 20;

    let head = client.head(request)?;
    let wire = format_h1_request(&head, None);
    let (status, headers, trailing) = timeout(CONTROL_TIMEOUT, async {
        socket.write_all(&wire).await.map_err(|_| Fault::Lost)?;
        read_h1_response_head(&mut socket).await
    })
    .await
    .unwrap_or(Err(Fault::TimedOut("download answer")))?;
    if let Some(fault) = client.refusal(request, status, &headers) {
        return Err(fault);
    }
    if headers.contains_key(header::TRANSFER_ENCODING) {
        // Stay on Hyper's keep-alive connection for future requests on this group.
        disable_direct.store(true, Ordering::Relaxed);
        let mut te = headers.get_all(header::TRANSFER_ENCODING).iter();
        let is_plain_chunked = te
            .next()
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("chunked"))
            && te.next().is_none();
        if is_plain_chunked {
            lane.announce(tally);
            drain_chunked(&mut socket, trailing, lane, tally).await?;
            return Ok(true);
        }
        return Ok(false);
    }
    let total = match parse_content_length_headers(&headers) {
        Ok(Some(len)) => len,
        Ok(None) => {
            // Close-delimited HTTP/1.1 response: consume this first body directly, then stay on Hyper.
            disable_direct.store(true, Ordering::Relaxed);
            lane.announce(tally);
            drain_close_delimited(&mut socket, trailing, lane, tally).await?;
            return Ok(true);
        }
        Err(()) => {
            disable_direct.store(true, Ordering::Relaxed);
            return Ok(false);
        }
    };
    lane.announce(tally);
    let initial = (trailing.len() as u64).min(total);
    if initial > 0 {
        tally.bytes.fetch_add(initial, Ordering::Relaxed);
        lane.moved = true;
    }
    let mut remaining = total - initial;
    // Linux `MSG_TRUNC` never writes to this buffer; uninitialized capacity touches zero physical pages.
    let mut discard: Vec<u8> = Vec::with_capacity(256 << 10);
    let mut bulk_read = if remaining > READ_LOWAT_MIN {
        BulkRead::sample()
    } else {
        BulkRead::Ordinary
    };
    let mut since_yield: u64 = 0;
    let mut wait_sleep = std::pin::pin!(tokio::time::sleep(BulkRead::WAIT));
    while remaining > 0 {
        if bulk_read.is_batched() && remaining <= bulk_read.lowat() {
            let _ = bulk_read.finish(&socket);
        }
        let cap = remaining.min(discard.capacity() as u64) as usize;
        let read = socket.try_io(Interest::READABLE, || {
            let (_, len) = recv(
                &socket,
                &mut discard.spare_capacity_mut()[..cap],
                RecvFlags::TRUNC,
            )?;
            Ok(len)
        });
        match read {
            Ok(0) => {
                let _ = bulk_read.finish(&socket);
                return Err(Fault::Lost);
            }
            Ok(n) => {
                let n = n.min(cap);
                tally.bytes.fetch_add(n as u64, Ordering::Relaxed);
                lane.moved = true;
                remaining -= n as u64;
                bulk_read.on_read(n);
                since_yield += n as u64;
                if since_yield >= YIELD_BYTES {
                    since_yield = 0;
                    tokio::task::yield_now().await;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                since_yield = 0;
                if !bulk_read.is_batched() {
                    bulk_read.on_would_block(&socket, remaining);
                }
                if bulk_read.is_batched() {
                    wait_sleep
                        .as_mut()
                        .reset(tokio::time::Instant::now() + BulkRead::WAIT);
                    tokio::select! {
                        biased;
                        ready = socket.readable() => {
                            if ready.is_err() {
                                let _ = bulk_read.finish(&socket);
                                return Err(Fault::Lost);
                            }
                        }
                        () = &mut wait_sleep => bulk_read.reset_after_timeout(&socket),
                    }
                } else if socket.readable().await.is_err() {
                    let _ = bulk_read.finish(&socket);
                    return Err(Fault::Lost);
                }
            }
            Err(_) => {
                let _ = bulk_read.finish(&socket);
                return Err(Fault::Lost);
            }
        }
    }
    let _ = bulk_read.finish(&socket);
    Ok(true)
}

/// Direct plain HTTP/1.1 upload bypassing hyper framing/buffering, preserving TCP autotuning on all platforms.
async fn direct_upload(
    mut socket: tokio::net::TcpStream,
    client: &Client,
    request: &Request,
    block: &[u8],
    lane: &mut Lane,
    tally: &Tally,
) -> Result<(), Fault> {
    use tokio::io::AsyncWriteExt;

    let head = client.head(request)?;
    let wire = format_h1_request(&head, Some(TRANSFER_BYTES));
    lane.announce(tally);
    let (mut reader, mut writer) = socket.split();
    let mut answer = std::pin::pin!(read_h1_response_head(&mut reader));
    let head_sent = tokio::select! {
        biased;
        res = &mut answer => {
            let (status, headers, _) = res?;
            if let Some(fault) = client.refusal(request, status, &headers) {
                return Err(fault);
            }
            return Ok(());
        }
        res = timeout(CONTROL_TIMEOUT, writer.write_all(&wire)) => res,
    };
    match head_sent {
        Ok(Ok(())) => {}
        Ok(Err(_)) => {
            if let Ok(Ok((status, headers, _))) = timeout(Duration::from_secs(1), &mut answer).await
                && let Some(fault) = client.refusal(request, status, &headers)
            {
                return Err(fault);
            }
            return Err(Fault::Lost);
        }
        Err(_) => return Err(Fault::TimedOut("upload answer")),
    }
    let mut remaining = TRANSFER_BYTES;
    let mut offset = 0usize;
    let mut write_failed = false;
    while remaining > 0 {
        let len = ((block.len() - offset) as u64).min(remaining) as usize;
        tokio::select! {
            biased;
            res = &mut answer => {
                let (status, headers, _) = res?;
                if let Some(fault) = client.refusal(request, status, &headers) {
                    return Err(fault);
                }
                return Ok(());
            }
            wrote = writer.write(&block[offset..offset + len]) => match wrote {
                Ok(0) => {
                    write_failed = true;
                    break;
                }
                Ok(n) => {
                    remaining -= n as u64;
                    offset = (offset + n) % block.len();
                    lane.sent.fetch_add(n as u64, Ordering::Relaxed);
                }
                Err(_) => {
                    write_failed = true;
                    break;
                }
            }
        }
    }
    let wait = if write_failed {
        Duration::from_secs(1)
    } else {
        CONTROL_TIMEOUT
    };
    match timeout(wait, answer).await {
        Ok(Ok((status, headers, _))) => {
            if let Some(fault) = client.refusal(request, status, &headers) {
                return Err(fault);
            }
            if write_failed {
                return Err(Fault::Lost);
            }
            Ok(())
        }
        Ok(Err(fault)) => Err(fault),
        Err(_) => Err(if write_failed {
            Fault::Lost
        } else {
            Fault::TimedOut("upload answer")
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphite_meter_http3::server;

    #[tokio::test]
    async fn a_lane_that_panics_fails_as_the_client_s_own_bug() {
        let mut tasks = JoinSet::new();
        tasks.spawn(async { panic!("a lane's bug") });
        let mut lanes = Lanes {
            tally: Arc::default(),
            tasks,
            count: 1,
            failed: None,
        };
        let failed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match lanes.health() {
                    LaneHealth::Failed(failure) => break failure,
                    _ => tokio::task::yield_now().await,
                }
            }
        });
        assert_eq!(failed.await.unwrap(), Fault::Internal.failure());
    }

    #[tokio::test]
    async fn a_request_a_goaway_left_unsent_opens_once_more_on_a_fresh_connection() {
        let identity = graphite_meter_testkit::Identity::generate().unwrap();
        let address = "127.0.0.1:0".parse().unwrap();
        let endpoint = noq::Endpoint::server(identity.quic_server(), address).unwrap();
        let port = endpoint.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut first = true;
            while let Some(incoming) = endpoint.accept().await {
                let mut connection = server::Connection::new(incoming.await.unwrap(), None);
                if std::mem::take(&mut first) {
                    connection.retire();
                }
                tokio::spawn(async move {
                    while let Ok(Some(request)) = connection.next().await {
                        tokio::spawn(async move {
                            let Ok((_, stream)) = request.resolve().await else {
                                return;
                            };
                            let mut send = stream.split().0;
                            let _ = send.send_response(http::Response::new(())).await;
                            let _ = send.finish().await;
                        });
                    }
                });
            }
        });
        let (client, origin) = (
            Client::new(true, Arc::new(graphite_meter_net::Pool::inline())),
            Origin::parse(&format!("https://127.0.0.1:{port}")).unwrap(),
        );
        let dial = || client.dial(&origin, Protocol::Http3, ReadBuffer::Fixed);
        let retired = dial().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while retired.usable() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the GOAWAY");
        let handed = Mutex::new(Some(retired));
        let conn = || async {
            let handed = lock(&handed).take();
            match handed {
                Some(conn) => Ok(conn),
                None => dial().await,
            }
        };
        let request = Request::new(Method::GET, &origin, Route::Probe);
        let (conn, _) = open(conn, &client, &request, Payload::empty)
            .await
            .expect("the fresh connection's answer");
        assert!(conn.usable());
    }

    #[tokio::test]
    async fn direct_head_reader_skips_informational_responses_coalesced_and_split() {
        use tokio::io::AsyncWriteExt;

        for coalesced in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut peer, _) = listener.accept().await.unwrap();
                let hint = b"HTTP/1.1 103 Early Hints\r\nLink: </style.css>; rel=preload\r\n\r\n";
                let final_head = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
                if coalesced {
                    let mut combined = Vec::with_capacity(hint.len() + final_head.len());
                    combined.extend_from_slice(hint);
                    combined.extend_from_slice(final_head);
                    peer.write_all(&combined).await.unwrap();
                } else {
                    peer.write_all(hint).await.unwrap();
                    peer.flush().await.unwrap();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    peer.write_all(final_head).await.unwrap();
                }
            });
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            let (status, headers, trailing) = read_h1_response_head(&mut client).await.unwrap();
            server.await.unwrap();
            assert_eq!(status, http::StatusCode::OK);
            assert_eq!(headers.get(http::header::CONTENT_LENGTH).unwrap(), "5");
            assert_eq!(trailing, b"hello");
        }
    }

    #[tokio::test]
    async fn direct_upload_handles_early_refusal_while_peer_holds_request_open() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hold = tokio::spawn(async move {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut head = [0u8; 1024];
            let _ = peer.read(&mut head).await;
            let _ = peer
                .write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\n\r\n",
                )
                .await;
            let _ = peer.shutdown().await;
            // Hold the read half open without draining the upload body.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let client = Client::new(false, Arc::new(graphite_meter_net::Pool::inline()));
        let origin = Origin::parse(&format!("http://127.0.0.1:{}", addr.port())).unwrap();
        let request = Request::new(Method::POST, &origin, Route::Upload);
        let socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let block = vec![0u8; 64 << 10];
        let mut lane = Lane {
            number: 0,
            moved: false,
            announced: false,
            sent: Arc::new(AtomicU64::new(0)),
        };
        let tally = Tally::default();

        let started = Instant::now();
        let err = direct_upload(socket, &client, &request, &block, &mut lane, &tally)
            .await
            .expect_err("early 429 refusal");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "early 429 should return immediately, took {:?}",
            started.elapsed()
        );
        assert!(
            matches!(
                err,
                Fault::Status {
                    status: http::StatusCode::TOO_MANY_REQUESTS,
                    retry_after: Some(_),
                    ..
                }
            ),
            "got {err:?}"
        );
        hold.abort();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn chunked_download_counts_decoded_bytes_without_duplicating_requests() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let served = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut peer, _)) = listener.accept().await else {
                    break;
                };
                let served = served.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    loop {
                        let mut head = Vec::new();
                        let ok = loop {
                            let Ok(n) = peer.read(&mut buf).await else {
                                break false;
                            };
                            if n == 0 {
                                break false;
                            }
                            head.extend_from_slice(&buf[..n]);
                            if head.windows(4).any(|w| w == b"\r\n\r\n") {
                                break true;
                            }
                        };
                        if !ok {
                            break;
                        }
                        served.fetch_add(1, Ordering::Relaxed);
                        let resp = concat!(
                            "HTTP/1.1 200 OK\r\n",
                            "Transfer-Encoding: chunked\r\n",
                            "\r\n",
                            "5;ext=1\r\nhello\r\n",
                            "6\r\n world\r\n",
                            "0\r\nX-Trailer: ok\r\n\r\n",
                        );
                        if peer.write_all(resp.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });

        let client = Client::new(false, Arc::new(graphite_meter_net::Pool::inline()));
        let origin = Origin::parse(&format!("http://127.0.0.1:{}", addr.port())).unwrap();
        let plan = GroupPlan {
            path: ThroughputPath {
                origin,
                transport: ThroughputTransport::FetchStream,
                protocol: Protocol::Http1,
            },
            direction: Direction::Down,
            carrier: Carrier::Dialed,
            lanes: 0..1,
        };
        let group = Group {
            client,
            plan,
            work: Work::Download,
            block: Bytes::new(),
            conn: Slot(Default::default()),
            session: Slot(Default::default()),
            direct_down_disabled: AtomicBool::new(false),
        };
        let tally = Tally::default();
        let mut lane = Lane {
            number: 0,
            announced: false,
            moved: false,
            sent: Arc::default(),
        };

        for _ in 0..4 {
            lane.moved = false;
            group.attempt(&mut lane, &tally).await.unwrap();
            assert!(lane.moved);
        }
        assert!(group.direct_down_disabled.load(Ordering::Relaxed));
        assert_eq!(requests.load(Ordering::Relaxed), 4);
        assert_eq!(tally.bytes.load(Ordering::Relaxed), 4 * 11);
        server.abort();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn close_delimited_download_counts_bytes_without_duplicating_requests() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let served = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut peer, _)) = listener.accept().await else {
                    break;
                };
                let served = served.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let mut head = Vec::new();
                    loop {
                        let Ok(n) = peer.read(&mut buf).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        head.extend_from_slice(&buf[..n]);
                        if head.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    served.fetch_add(1, Ordering::Relaxed);
                    let resp = concat!(
                        "HTTP/1.1 200 OK\r\n",
                        "Connection: close\r\n",
                        "\r\n",
                        "hello world",
                    );
                    let _ = peer.write_all(resp.as_bytes()).await;
                    let _ = peer.shutdown().await;
                });
            }
        });

        let client = Client::new(false, Arc::new(graphite_meter_net::Pool::inline()));
        let origin = Origin::parse(&format!("http://127.0.0.1:{}", addr.port())).unwrap();
        let plan = GroupPlan {
            path: ThroughputPath {
                origin,
                transport: ThroughputTransport::FetchStream,
                protocol: Protocol::Http1,
            },
            direction: Direction::Down,
            carrier: Carrier::Dialed,
            lanes: 0..1,
        };
        let group = Group {
            client,
            plan,
            work: Work::Download,
            block: Bytes::new(),
            conn: Slot(Default::default()),
            session: Slot(Default::default()),
            direct_down_disabled: AtomicBool::new(false),
        };
        let tally = Tally::default();
        let mut lane = Lane {
            number: 0,
            announced: false,
            moved: false,
            sent: Arc::default(),
        };

        for _ in 0..4 {
            lane.moved = false;
            group.attempt(&mut lane, &tally).await.unwrap();
            assert!(lane.moved);
        }
        assert!(group.direct_down_disabled.load(Ordering::Relaxed));
        assert_eq!(requests.load(Ordering::Relaxed), 4);
        assert_eq!(tally.bytes.load(Ordering::Relaxed), 4 * 11);
        server.abort();
    }
}

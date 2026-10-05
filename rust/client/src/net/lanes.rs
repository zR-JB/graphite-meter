//! Lane groups: which connection or session a stage's lanes share, and the one loop every lane runs.
use super::{
    Attempt, Client, Request, Retry,
    conn::{Conn, Payload, ReadBuffer, TRANSFER_BYTES},
    fault::Fault,
    lock,
    session::Session,
};
use crate::model::{Dir, Direction, Failure, LaneHealth, Stage};
use bytes::Bytes;
use graphite_meter_proto::{
    discovery::{Protocol, ThroughputTransport},
    origin::Origin,
    refusal::UploadRefusal,
    route::Route,
};
use http::Method;
use std::{
    future::Future,
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{runtime::Handle, sync::watch, task::JoinSet};
use tokio_util::sync::CancellationToken;

/// The bytes each WebTransport download stream carries.
const STREAM_BYTES: u64 = 64 << 20;
/// The most lanes one WebTransport session carries.
const SESSION_LANES: usize = 16;
/// The block an upload repeats; an attempt that handed on more than one moved bytes.
const BLOCK_BYTES: usize = 64 << 10;

/// A server's throughput path as preparation chose it, its protocol resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThroughputPath {
    pub origin: Origin,
    pub transport: ThroughputTransport,
    pub protocol: Protocol,
}

/// What a group's lanes share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    /// A connection the group dials: HTTP/1.1 for one lane, HTTP/2 or HTTP/3 for many.
    Dialed,
    /// The path's HTTP/3 connection, which control requests use too.
    Path,
    /// A WebTransport session on a connection of its own.
    Session,
}

/// One lane group: what its lanes share, their direction and their numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPlan {
    pub path: ThroughputPath,
    pub direction: Direction,
    pub carrier: Carrier,
    pub lanes: Range<usize>,
}

/// The groups `lanes` form in `stage` on `path`: HTTP/1.1 one connection per lane, HTTP/2 one per direction,
/// HTTP/3 the path's connection except for a bidirectional stage's uploads, WebTransport a session per 16 lanes.
pub fn topology(path: &ThroughputPath, stage: Stage, lanes: Dir<usize>) -> Vec<GroupPlan> {
    let mut plans = Vec::new();
    for &direction in stage.directions() {
        let apart = stage == Stage::Bidirectional && direction == Direction::Up;
        let (carrier, size) = match (path.transport, path.protocol) {
            (ThroughputTransport::FetchStream, Protocol::Http2) => (Carrier::Dialed, usize::MAX),
            (ThroughputTransport::FetchStream, Protocol::Http3) if apart => (Carrier::Dialed, usize::MAX),
            (ThroughputTransport::FetchStream, Protocol::Http3) => (Carrier::Path, usize::MAX),
            (ThroughputTransport::FetchStream, _) => (Carrier::Dialed, 1),
            _ => (Carrier::Session, SESSION_LANES),
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

/// What a direction's lanes do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Work {
    Download,
    /// Send to the receiver of this upload ID.
    Upload(String),
}

/// A direction's lanes in their groups; dropping it ends them.
pub struct Lanes {
    tally: Arc<Tally>,
    tasks: JoinSet<Option<Fault>>,
    count: usize,
    failed: Option<Fault>,
}

/// What a direction's lanes report, shared across their runtimes.
#[derive(Default)]
struct Tally {
    bytes: AtomicU64,
    ready: AtomicUsize,
    /// Each lane's failure while it retries without moving bytes.
    retrying: Mutex<Vec<Option<Failure>>>,
    /// The latest session upload lanes opened.
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
    /// Starts `work` on the plans in its direction, lane `n` after `n` staggers, each group on one pinned runtime,
    /// until `token` is cancelled.
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
        let plans: Vec<_> = plans.into_iter().filter(|plan| plan.direction == direction).collect();
        let count = plans.iter().map(|plan| plan.lanes.len()).sum();
        let tally = Arc::new(Tally { retrying: Mutex::new(vec![None; count]), ..Tally::default() });
        let block = if direction == Direction::Up { block() } else { Bytes::new() };
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
            };
            let group = Arc::new(group);
            for lane in group.plan.lanes.clone() {
                let (group, tally, token) = (group.clone(), tally.clone(), token.clone());
                let delay = stagger.saturating_mul(u32::try_from(lane).unwrap_or(u32::MAX));
                tasks.spawn_on(async move { token.run_until_cancelled(group.run(lane, tally, delay)).await }, &home);
            }
        }
        Self { tally, tasks, count, failed: None }
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
                Err(error) => Fault::Lost(error.to_string()),
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

    /// The upload refusal a lane's fault stood for, once `health` saw it.
    pub fn refusal(&self) -> Option<UploadRefusal> {
        match self.failed {
            Some(Fault::Refused(refusal)) => Some(refusal),
            _ => None,
        }
    }

    /// The latest session WebTransport upload lanes opened, whose first server stream carries the receiver's feed.
    pub fn session(&self) -> watch::Receiver<Option<Arc<Session>>> {
        self.tally.session.subscribe()
    }
}

/// A random block, so no hop compresses the upload; zeros only where the platform has no randomness.
fn block() -> Bytes {
    let mut block = vec![0; BLOCK_BYTES];
    let _ = getrandom::fill(&mut block);
    block.into()
}

/// A group's shared connection or session, replaced one dial at a time once unusable.
struct Slot<T>(tokio::sync::Mutex<Option<T>>);

impl<T> Slot<T> {
    /// A `handle` to the one `usable` says may go on, else to what `dial` brings; without a handle, as for an
    /// HTTP/1.1 connection, the lane keeps it to itself.
    async fn get(
        &self,
        dial: impl Future<Output = Result<T, Fault>>,
        usable: fn(&T) -> bool,
        handle: fn(&T) -> Option<T>,
    ) -> Result<T, Fault> {
        let mut current = self.0.lock().await;
        if let Some(handle) = current.as_ref().filter(|shared| usable(shared)).and_then(handle) {
            return Ok(handle);
        }
        let dialed = dial.await?;
        let Some(handle) = handle(&dialed) else { return Ok(dialed) };
        *current = Some(dialed);
        Ok(handle)
    }
}

/// A group: its plan and what its lanes share, a connection or a session as the plan's carrier says.
struct Group {
    client: Client,
    plan: GroupPlan,
    work: Work,
    /// The block uploads repeat.
    block: Bytes,
    conn: Slot<Conn>,
    session: Slot<Arc<Session>>,
}

/// One lane's state across its attempts.
struct Lane {
    number: usize,
    announced: bool,
    /// The attempt consumed payload.
    moved: bool,
    /// Bytes its upload bodies handed on.
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
    /// Runs lane `number` after `delay` until its fault stands.
    async fn run(self: Arc<Self>, number: usize, tally: Arc<Tally>, delay: Duration) -> Fault {
        tokio::time::sleep(delay).await;
        let mut lane = Lane { number, announced: false, moved: false, sent: Arc::default() };
        let mut retry = Retry::default();
        loop {
            let (started, sent) = (Instant::now(), lane.sent.load(Ordering::Relaxed));
            lane.moved = false;
            let result = self.attempt(&mut lane, &tally).await;
            let moved = lane.moved || lane.sent.load(Ordering::Relaxed) - sent > BLOCK_BYTES as u64;
            let fault = match result {
                Ok(()) if moved => {
                    retry = Retry::default();
                    tally.retrying(number, None);
                    continue;
                }
                Ok(()) => Fault::TimedOut("transfer"),
                Err(fault) => fault,
            };
            let moved = moved && !matches!(fault, Fault::Status { .. } | Fault::SignIn(_) | Fault::Refused(_));
            tally.retrying(number, (!moved).then(|| fault.failure()));
            match retry.after(fault, Attempt { started, moved }, Instant::now()) {
                Ok(pause) => tokio::time::sleep(pause).await,
                Err(fault) => return fault,
            }
        }
    }

    async fn attempt(&self, lane: &mut Lane, tally: &Tally) -> Result<(), Fault> {
        let (ThroughputPath { origin, protocol, .. }, client) = (&self.plan.path, &self.client);
        let conn = match self.plan.carrier {
            Carrier::Session => {
                let (dial, usable) = (self.dial_session(tally), |session: &Arc<Session>| session.usable());
                let session = self.session.get(dial, usable, |session| Some(session.clone())).await?;
                return match &self.work {
                    Work::Download => receive(&session, lane, tally).await,
                    Work::Upload(_) => send(&session, &self.block, lane, tally).await,
                };
            }
            Carrier::Dialed => {
                let down = self.plan.direction == Direction::Down;
                let buffer = if down { ReadBuffer::Fixed } else { ReadBuffer::Adaptive };
                let dial = client.dial(origin, *protocol, buffer);
                self.conn.get(dial, Conn::usable, Conn::share).await?
            }
            Carrier::Path => client.connections.shared(client, origin, *protocol).await?,
        };
        match &self.work {
            Work::Download => self.download(conn, lane, tally).await,
            Work::Upload(id) => self.upload(conn, id, lane, tally).await,
        }
    }

    /// The group's session, dialed on the lane's runtime; an upload's is announced for its feed.
    async fn dial_session(&self, tally: &Tally) -> Result<Arc<Session>, Fault> {
        let (route, query) = match &self.work {
            Work::Download => {
                let streams = self.plan.lanes.len().to_string();
                (Route::WtDownload, vec![("bytes", STREAM_BYTES.to_string()), ("streams", streams)])
            }
            Work::Upload(id) => (Route::WtUpload, vec![("id", id.clone())]),
        };
        let home = Handle::current();
        let session = self.client.session_on(&home, &self.plan.path.origin, route, query);
        let session = Arc::new(session.await?);
        if let Work::Upload(_) = self.work {
            tally.session.send_replace(Some(session.clone()));
        }
        Ok(session)
    }

    fn request(&self, method: Method, route: Route, query: Vec<(&'static str, String)>) -> Request {
        Request { method, origin: self.plan.path.origin.clone(), route, query }
    }

    async fn download(&self, mut conn: Conn, lane: &mut Lane, tally: &Tally) -> Result<(), Fault> {
        let query = vec![
            ("bytes", TRANSFER_BYTES.to_string()),
            ("cb", cache_buster()),
            ("lane", lane.number.to_string()),
        ];
        let request = self.request(Method::GET, Route::Download, query);
        let mut body = conn.open(&self.client, &request, Payload::empty()).await?;
        lane.announce(tally);
        while let Some(chunk) = body.chunk().await? {
            tally.bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            lane.moved |= !chunk.is_empty();
        }
        Ok(())
    }

    async fn upload(&self, mut conn: Conn, id: &str, lane: &mut Lane, tally: &Tally) -> Result<(), Fault> {
        let query = vec![("cb", cache_buster()), ("id", id.to_owned()), ("lane", lane.number.to_string())];
        let request = self.request(Method::POST, Route::Upload, query);
        lane.announce(tally);
        let payload = Payload::repeat(self.block.clone(), lane.sent.clone());
        let mut answer = conn.open(&self.client, &request, payload).await?;
        while answer.chunk().await?.is_some() {}
        Ok(())
    }
}

/// One server stream of the session, counted as it arrives; the lane opens at its first bytes.
async fn receive(session: &Session, lane: &mut Lane, tally: &Tally) -> Result<(), Fault> {
    let mut stream = session.accept_uni().await?;
    let mut received = 0;
    while let Some(chunk) = stream.read_chunk().await.map_err(|error| session.fault(error))? {
        let size = chunk.len() as u64;
        received += size;
        tally.bytes.fetch_add(size, Ordering::Relaxed);
        if size > 0 {
            lane.moved = true;
            lane.announce(tally);
        }
        if received > STREAM_BYTES {
            return Err(Fault::Malformed("WebTransport download exceeded its byte count".into()));
        }
    }
    match received == STREAM_BYTES {
        true => Ok(()),
        false => Err(Fault::Lost("WebTransport download ended before its byte count".into())),
    }
}

/// Streams of a transfer's bytes, one after another, on the session.
async fn send(session: &Session, block: &Bytes, lane: &mut Lane, tally: &Tally) -> Result<(), Fault> {
    loop {
        let mut stream = session.open_uni().await?;
        lane.announce(tally);
        let mut payload = Payload::repeat(block.clone(), lane.sent.clone());
        while let Some(slice) = payload.slice() {
            stream.write_chunk(slice).await.map_err(|error| session.fault(error))?;
        }
        stream.finish().map_err(|error| session.fault(error))?;
    }
}

/// A lane request's cache buster: the time in nanoseconds.
fn cache_buster() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH);
    now.unwrap_or_default().as_nanos().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plans(transport: ThroughputTransport, protocol: Protocol, stage: Stage, down: usize, up: usize) -> Vec<String> {
        let origin = Origin::parse("https://meter.example").unwrap();
        let path = ThroughputPath { origin, transport, protocol };
        let plans = topology(&path, stage, Dir { down, up });
        plans
            .iter()
            .map(|plan| format!("{:?} {:?} {:?}", plan.direction, plan.carrier, plan.lanes))
            .collect()
    }

    #[test]
    fn topology_follows_the_measured_connections_per_protocol_and_stage() {
        use {Protocol::*, Stage::*, ThroughputTransport::*};
        type Row = (ThroughputTransport, Protocol, Stage, usize, usize, &'static [&'static str]);
        let rows: &[Row] = &[
            (
                FetchStream,
                Http1,
                Download,
                3,
                3,
                &["Down Dialed 0..1", "Down Dialed 1..2", "Down Dialed 2..3"],
            ),
            (FetchStream, Http1, Upload, 0, 2, &["Up Dialed 0..1", "Up Dialed 1..2"]),
            (FetchStream, Http1, Latency, 6, 6, &[]),
            (FetchStream, Http2, Bidirectional, 1, 4, &["Down Dialed 0..1", "Up Dialed 0..4"]),
            (FetchStream, Http3, Download, 2, 1, &["Down Path 0..2"]),
            (FetchStream, Http3, Upload, 1, 3, &["Up Path 0..3"]),
            (FetchStream, Http3, Bidirectional, 1, 1, &["Down Path 0..1", "Up Dialed 0..1"]),
            (FetchStream, Negotiated, Upload, 1, 2, &["Up Dialed 0..1", "Up Dialed 1..2"]),
            (WebTransport, Http3, Download, 14, 1, &["Down Session 0..14"]),
            (
                WebTransport,
                Http3,
                Bidirectional,
                40,
                16,
                &["Down Session 0..16", "Down Session 16..32", "Down Session 32..40", "Up Session 0..16"],
            ),
        ];
        for (transport, protocol, stage, down, up, expected) in rows {
            let plans = plans(*transport, *protocol, *stage, *down, *up);
            assert_eq!(plans, *expected, "{transport:?} {protocol:?} {stage:?} {down}/{up}");
        }
    }
}

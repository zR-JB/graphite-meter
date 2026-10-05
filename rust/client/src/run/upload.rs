//! An upload session (`api/upload.md`): a receiver minted for the stage, its lanes and progress feed, checkpoints,
//! one replacement per server and run, and its finish.
use crate::{
    measure::aggregate::{Fed, Receiver},
    model::LaneHealth,
    net::{
        Attempt, CONTROL_TIMEOUT, Class, Client, Decode, Fault, GroupPlan, Incoming, Lanes, Request, Retry, Session,
        ThroughputPath, Work, retrying,
    },
};
use bytes::Bytes;
use graphite_meter_http3::webtransport::RecvStream;
use graphite_meter_proto::{
    discovery::{Protocol, ThroughputTransport},
    json,
    origin::Origin,
    refusal::UploadRefusal,
    route::Route,
    upload::{Counters, MAX_RECORD_BYTES, Record, Session as Minted},
};
use http::Method;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{Instant, sleep, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;

/// The pause before a missed checkpoint is asked again.
const CHECKPOINT_RETRY: Duration = Duration::from_millis(100);

/// One stage's uploads to a server; dropping it ends its lanes and feed.
pub struct UploadSession {
    start: Start,
    upload: Upload,
    replacing: Option<JoinHandle<Option<Result<Upload, Fault>>>>,
    /// The server's one replacement receiver in this run was taken.
    replaced: Arc<AtomicBool>,
}

/// What a receiver starts from, so that a replacement starts the same lanes.
#[derive(Clone)]
struct Start {
    /// The client its lanes go through.
    client: Client,
    control: Control,
    plans: Vec<GroupPlan>,
    stagger: Duration,
    webtransport: bool,
    token: CancellationToken,
}

/// A receiver: its upload ID, its number in the session, its lanes and what its feed reported.
struct Upload {
    id: String,
    number: u32,
    lanes: Lanes,
    progress: watch::Receiver<Progress>,
    /// Ends its lanes and feed.
    token: CancellationToken,
}

#[derive(Debug, Default)]
struct Progress {
    ready: bool,
    latest: Option<Counters>,
    complete: bool,
    /// The fault the feed ended with after recovery.
    ended: Option<Fault>,
}

/// Where control requests go: the client's pool, or for an HTTP/3 path connections of the session's own.
#[derive(Clone)]
struct Control {
    client: Client,
    via: Protocol,
    origin: Origin,
}

impl UploadSession {
    /// Mints a receiver and starts its feed and its lanes `stagger` apart; `replaced` is the server's for the run.
    pub async fn open(
        client: &Client,
        path: &ThroughputPath,
        plans: Vec<GroupPlan>,
        stagger: Duration,
        replaced: Arc<AtomicBool>,
        token: CancellationToken,
    ) -> Result<Self, Fault> {
        let origin = path.origin.clone();
        let control = match path.protocol {
            Protocol::Http3 => Control { client: client.apart(), via: Protocol::Http3, origin },
            _ => Control { client: client.clone(), via: Protocol::Negotiated, origin },
        };
        let webtransport = path.transport == ThroughputTransport::WebTransport;
        let start = Start {
            client: client.clone(),
            control,
            plans,
            stagger,
            webtransport,
            token,
        };
        let upload = start.clone().upload(0).await?;
        Ok(Self { start, upload, replacing: None, replaced })
    }

    /// Lanes open, the feed attached and the receiver counting.
    pub fn ready(&self) -> bool {
        let progress = self.upload.progress.borrow();
        let counting = matches!(progress.latest, Some(latest) if latest.bytes() > 0 && latest.nanos() > 0);
        self.upload.lanes.ready() && progress.ready && counting
    }

    /// The highest byte count the current receiver's feed reported.
    pub fn fed(&self) -> Option<Fed> {
        let latest = self.upload.progress.borrow().latest?;
        Some(Fed { id: self.upload.number, bytes: latest.bytes() })
    }

    /// A fresh checkpoint within `budget`, a miss asked again every 100 ms unless its fault is final.
    pub async fn checkpoint(&self, budget: Duration) -> Result<Receiver, Fault> {
        let (deadline, id, control) = (Instant::now() + budget, Some(self.upload.id.as_str()), &self.start.control);
        loop {
            let asked = control.json(Method::POST, Route::UploadCheckpoint, id, json::decode::<Counters>);
            let fault = match timeout_at(deadline, asked).await {
                Ok(Ok(counters)) => return Ok(Receiver { id: self.upload.number, counters }),
                Ok(Err(fault)) => fault,
                Err(_) => return Err(Fault::TimedOut("receiver checkpoint")),
            };
            if fault.class() == Class::Final || deadline.saturating_duration_since(Instant::now()) <= CHECKPOINT_RETRY {
                return Err(fault);
            }
            sleep(CHECKPOINT_RETRY).await;
        }
    }

    /// The lanes' and feed's health; a receiver that no longer knows its ID is replaced once per server and run.
    pub fn health(&mut self) -> LaneHealth {
        if let Some(replacing) = self.replacing.take_if(|replacing| replacing.is_finished()) {
            match futures_util::FutureExt::now_or_never(replacing) {
                Some(Ok(Some(Ok(upload)))) => self.upload = upload,
                Some(Ok(Some(Err(fault)))) => return LaneHealth::Failed(fault.failure()),
                _ => return LaneHealth::Failed(Fault::Lost("upload replacement ended".into()).failure()),
            }
        }
        if self.replacing.is_some() {
            return LaneHealth::Ok;
        }
        let lanes = self.upload.lanes.health();
        let progress = self.upload.progress.borrow();
        let feed = progress.ended.as_ref();
        let invalid = matches!(feed, Some(Fault::Refused(UploadRefusal::Invalid)));
        let invalid = invalid || self.upload.lanes.refusal() == Some(UploadRefusal::Invalid);
        if invalid && !self.replaced.swap(true, Ordering::Relaxed) {
            drop(progress);
            self.replace();
            return LaneHealth::Ok;
        }
        feed.map_or(lanes, |fault| LaneHealth::Failed(fault.failure()))
    }

    /// Ends the lanes, then asks the receiver to finalize and waits for its `complete` record, within `budget`.
    pub async fn finish(self, budget: Duration) {
        let Upload { id, lanes, mut progress, token, .. } = self.upload;
        drop(lanes);
        let finished = async {
            let control = &self.start.control;
            if let Ok(mut answer) = control.send(Method::DELETE, Route::UploadProgress, Some(&id)).await {
                while let Ok(Some(_)) = answer.chunk().await {}
            }
            let _ = progress
                .wait_for(|progress| progress.complete || progress.ended.is_some())
                .await;
        };
        let _ = timeout(budget, finished).await;
        token.cancel();
        self.start.token.cancel();
    }

    /// Ends the current receiver and starts another in its place.
    fn replace(&mut self) {
        self.upload.token.cancel();
        let (start, number) = (self.start.clone(), self.upload.number + 1);
        let token = start.token.clone();
        self.replacing = Some(tokio::spawn(token.run_until_cancelled_owned(start.upload(number))));
    }
}

impl Start {
    /// Mints receiver `number` and starts its feed and lanes.
    async fn upload(self, number: u32) -> Result<Upload, Fault> {
        let control = &self.control;
        let mint = || control.json(Method::POST, Route::UploadSession, None, Minted::decode);
        let id = retrying(mint).await?.upload_id;
        let token = self.token.child_token();
        let work = Work::Upload(id.clone());
        let lanes = Lanes::start(&self.client, self.plans, work, self.stagger, token.child_token());
        let (feed, progress) = watch::channel(Progress::default());
        let session = self.webtransport.then(|| lanes.session());
        let follow = follow(self.control.clone(), id.clone(), session, feed);
        tokio::spawn(token.clone().run_until_cancelled_owned(follow));
        Ok(Upload { id, number, lanes, progress, token })
    }
}

impl Control {
    fn request(&self, method: Method, route: Route, id: Option<&str>) -> Request {
        let mut request = Request::new(method, &self.origin, route);
        request.query.extend(id.map(|id| ("id", id.to_owned())));
        request
    }

    /// The answer's head to `method` on `route` for upload `id`, if any, within the control timeout.
    async fn send(&self, method: Method, route: Route, id: Option<&str>) -> Result<Incoming, Fault> {
        self.client.control(self.via, self.request(method, route, id)).await
    }

    /// The JSON answer to `method` on `route` for upload `id`, if any, within the control timeout.
    async fn json<T>(&self, method: Method, route: Route, id: Option<&str>, decode: Decode<T>) -> Result<T, Fault> {
        self.client
            .json(self.via, self.request(method, route, id), decode)
            .await
    }
}

/// Follows upload `id`'s feed into `progress` until `complete`: the WebTransport session's first server stream when
/// there is one, else or once that fails the HTTP feed, reopened as the retry rule allows.
async fn follow(
    control: Control,
    id: String,
    session: Option<watch::Receiver<Option<Arc<Session>>>>,
    progress: watch::Sender<Progress>,
) {
    if let Some(mut sessions) = session {
        let streamed = async {
            let session = sessions
                .wait_for(Option::is_some)
                .await
                .ok()
                .and_then(|session| session.clone());
            let session = session.ok_or_else(|| Fault::Lost("upload lanes ended".into()))?;
            let stream = session.accept_uni().await?;
            read(&control, Source::Stream(stream, session), &progress, &mut false).await
        };
        match streamed.await {
            Ok(()) => return,
            Err(fault) if fault.class() == Class::Final => {
                return progress.send_modify(|state| state.ended = Some(fault));
            }
            Err(_) => {}
        }
    }
    let mut retry = Retry::default();
    loop {
        let (started, mut moved) = (std::time::Instant::now(), false);
        let feed = control.send(Method::GET, Route::UploadProgress, Some(&id)).await;
        let fault = match feed {
            Ok(feed) => match read(&control, Source::Http(feed), &progress, &mut moved).await {
                Ok(()) => return,
                Err(fault) => fault,
            },
            Err(fault) => fault,
        };
        match retry.after(fault, Attempt { started, moved }, std::time::Instant::now()) {
            Ok(pause) => sleep(pause).await,
            Err(fault) => return progress.send_modify(|state| state.ended = Some(fault)),
        }
    }
}

/// Where the feed's lines arrive.
enum Source {
    Http(Incoming),
    Stream(RecvStream, Arc<Session>),
}

impl Source {
    async fn chunk(&mut self) -> Result<Option<Bytes>, Fault> {
        match self {
            Self::Http(feed) => feed.chunk().await,
            Self::Stream(stream, session) => stream.read_chunk().await.map_err(|error| session.fault(error)),
        }
    }
}

/// Records from `source` into `progress` until `complete`, each line at most 64 KiB and within the control timeout;
/// `moved` once one decoded.
async fn read(
    control: &Control,
    mut source: Source,
    progress: &watch::Sender<Progress>,
    moved: &mut bool,
) -> Result<(), Fault> {
    let mut line = Vec::new();
    loop {
        let chunk = timeout(CONTROL_TIMEOUT, source.chunk()).await;
        let chunk = chunk.unwrap_or(Err(Fault::TimedOut("upload progress")))?;
        let mut chunk = chunk.ok_or_else(|| Fault::Lost("upload progress ended without complete".into()))?;
        while !chunk.is_empty() {
            let end = chunk.iter().position(|&byte| byte == b'\n');
            let taken = chunk.split_to(end.map_or(chunk.len(), |end| end + 1));
            if line.len() + taken.len() > MAX_RECORD_BYTES {
                return Err(Fault::Malformed("upload progress record exceeds 64 KiB".into()));
            }
            line.extend_from_slice(&taken);
            if end.is_none() {
                continue;
            }
            let record = Record::decode(&line[..line.len() - 1]);
            line.clear();
            if let Ok(record) = record {
                *moved = true;
                if apply(control, record, progress)? {
                    return Ok(());
                }
            }
        }
    }
}

/// Applies a record; true once `complete` arrived. A regressing observation is stale and ignored.
fn apply(control: &Control, record: Record, progress: &watch::Sender<Progress>) -> Result<bool, Fault> {
    let (counters, complete) = match record {
        Record::Ready => {
            progress.send_modify(|state| state.ready = true);
            return Ok(false);
        }
        Record::Progress(counters) => (counters, false),
        Record::Complete(counters) => (counters, true),
        Record::Error { code, .. } => return Err(control.client.upload_error(&control.origin, &code)),
    };
    let fresh = progress.borrow().latest.is_none_or(|latest| counters.follows(latest));
    if fresh {
        progress.send_modify(|state| (state.latest, state.complete) = (Some(counters), complete));
    }
    Ok(fresh && complete)
}

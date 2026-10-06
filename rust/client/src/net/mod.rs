//! The network layer: one request API over HTTP/1.1, 2 and 3, lane groups, latency bus, failures, retry, sign-in.
pub mod approval;
mod bus;
mod conn;
mod fault;
mod lanes;
mod retry;

pub use bus::{Bus, LatencyPath};
pub use conn::{Conn, Decode, Incoming, Payload, ReadBuffer, Session};
pub use fault::{Class, Fault};
pub use lanes::{Carrier, GroupPlan, Lanes, ThroughputPath, Work, topology};
pub use retry::{Attempt, REDIAL_WINDOW, Retry, retrying};

use conn::{Answer, Failed};
use graphite_meter_net::{Connector, Pool, Proxy, Verify};
use graphite_meter_proto::{
    discovery::{Probe, Protocol},
    lane::LaneEnding,
    origin::{Origin, Scheme},
    refusal::UploadRefusal,
    route::Route,
};
use http::{HeaderMap, HeaderValue, Method, StatusCode, Version, header};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    runtime::Handle,
    time::{Instant, timeout_at},
};

/// How long a control request may take, its answer's body included, and a session or bus to open.
pub(crate) const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);

/// A path check's network state, reused by its run: proxies, verification, grants, control connections, QUIC runtimes.
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
    connections: Arc<Connections>,
}

struct Shared {
    connector: Connector,
    verify: Verify,
    grants: Mutex<Grants>,
    runtimes: Arc<Pool>,
}

/// Each signed-in server's grant, and the server whose grant each enrolled target takes.
#[derive(Default)]
struct Grants {
    tokens: HashMap<Origin, HeaderValue>,
    issuers: HashMap<Origin, Origin>,
}

impl Grants {
    /// The server whose grant `target` takes: the one that enrolled it, else its own.
    fn issuer<'a>(&'a self, target: &'a Origin) -> &'a Origin {
        self.issuers.get(target).unwrap_or(target)
    }
}

/// A bodyless request to a route.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub origin: Origin,
    pub route: Route,
    pub query: Vec<(&'static str, String)>,
}

impl Request {
    pub fn new(method: Method, origin: &Origin, route: Route) -> Self {
        Self { method, origin: origin.clone(), route, query: Vec::new() }
    }
}

impl Client {
    /// A client through the environment's proxies; `insecure` skips certificate checks and never sends a grant.
    pub fn new(insecure: bool, runtimes: Arc<Pool>) -> Self {
        Self::with(if insecure { Verify::Insecure } else { Verify::trusted() }, Proxy::from_env(), runtimes)
    }

    /// A client through `proxy`, checking certificates as `verify` says; an insecure one never sends a grant.
    pub fn with(verify: Verify, proxy: Proxy, runtimes: Arc<Pool>) -> Self {
        let connector = Connector::new(proxy, verify.clone());
        let shared = Arc::new(Shared { connector, verify, grants: Mutex::default(), runtimes });
        Self { shared, connections: Arc::default() }
    }

    /// The same client with control connections of its own, apart from those its lanes may share.
    pub fn apart(&self) -> Self {
        Self { shared: self.shared.clone(), connections: Arc::default() }
    }

    /// A control request's JSON answer, read by `decode` within the control timeout.
    pub async fn json<T>(&self, via: Protocol, request: Request, decode: Decode<T>) -> Result<T, Fault> {
        Ok(self.exchange(via, request, decode).await?.1)
    }

    /// The protocol a valid probe answer from `origin` came over, a negotiated one resolved.
    pub async fn probe(&self, origin: &Origin, via: Protocol) -> Result<Protocol, Fault> {
        let request = Request::new(Method::GET, origin, Route::Probe);
        match (via, self.exchange(via, request, Probe::decode).await?.0) {
            (Protocol::Negotiated, Version::HTTP_11) => Ok(Protocol::Http1),
            (Protocol::Negotiated, Version::HTTP_2) => Ok(Protocol::Http2),
            (Protocol::Negotiated, _) => Err(Fault::Malformed("probe used an unsupported HTTP protocol".into())),
            (via, _) => Ok(via),
        }
    }

    /// A control request's answer, its head within the control timeout; the caller bounds its body.
    pub async fn control(&self, via: Protocol, request: Request) -> Result<Incoming, Fault> {
        let deadline = Instant::now() + CONTROL_TIMEOUT;
        let answer = self.connections.send(self, via, &request, deadline).await?;
        self.check(&request, answer)
    }

    /// A connection of its own to `origin`, dialed on the current runtime, which then runs it.
    pub async fn dial(&self, origin: &Origin, via: Protocol, buffer: ReadBuffer) -> Result<Conn, Fault> {
        Conn::dial(self, origin, via, buffer, Some(&Handle::current())).await
    }

    /// Keeps `issuer`'s grant for its and its enrolled targets' requests; a token no header can carry is dropped.
    pub fn grant(&self, issuer: &Origin, token: &str) {
        if let Ok(mut value) = HeaderValue::from_str(&format!("Bearer {token}")) {
            value.set_sensitive(true);
            lock(&self.shared.grants).tokens.insert(issuer.clone(), value);
        }
    }

    /// Lets `server`'s grant reach its HTTPS `targets` on its host; one another server enrolled first keeps that grant.
    pub fn enroll(&self, server: &Origin, targets: &[Origin]) {
        let mut grants = lock(&self.shared.grants);
        grants.issuers.insert(server.clone(), server.clone());
        for target in targets {
            if target.scheme == Scheme::Https && target.host == server.host {
                grants.issuers.entry(target.clone()).or_insert_with(|| server.clone());
            }
        }
    }

    /// The HTTP version a JSON answer came over, and the answer read by `decode`, within the control timeout.
    async fn exchange<T>(&self, via: Protocol, request: Request, decode: Decode<T>) -> Result<(Version, T), Fault> {
        let deadline = Instant::now() + CONTROL_TIMEOUT;
        let exchange = async {
            let answer = self.connections.send(self, via, &request, deadline).await?;
            let version = answer.version();
            Ok((version, self.check(&request, answer)?.json(decode).await?))
        };
        let exchanged = timeout_at(deadline, exchange).await;
        exchanged.unwrap_or(Err(Fault::TimedOut("control response")))
    }

    /// The request's head with an absolute URI, uncacheable, with its server's grant over verified HTTPS.
    fn head(&self, request: &Request) -> Result<http::Request<()>, Fault> {
        let mut target = format!("{}{}", request.origin, request.route.path());
        if !request.query.is_empty() {
            let mut query = form_urlencoded::Serializer::new(String::new());
            target = format!("{target}?{}", query.extend_pairs(&request.query).finish());
        }
        let mut head = http::Request::builder()
            .method(request.method.clone())
            .uri(target)
            .header(header::CACHE_CONTROL, "no-store");
        let grants = lock(&self.shared.grants);
        let token = grants.tokens.get(grants.issuer(&request.origin));
        if let Some(token) =
            token.filter(|_| matches!(self.shared.verify, Verify::Trusted(_)) && request.origin.scheme == Scheme::Https)
        {
            head = head.header(header::AUTHORIZATION, token.clone());
        }
        head.body(()).map_err(|error| Fault::Malformed(error.to_string()))
    }

    /// The answer's body when it is a 200.
    fn check(&self, request: &Request, answer: Answer) -> Result<Incoming, Fault> {
        let refused = self.refusal(request, answer.status(), answer.headers());
        refused.map_or_else(|| Ok(answer.into_body()), Err)
    }

    /// The fault an upload progress `error` record from `origin` means; a revoked grant is dropped.
    pub fn upload_error(&self, origin: &Origin, code: &str) -> Fault {
        let Some(refusal) = UploadRefusal::from_name(code) else {
            return Fault::Malformed(format!("upload progress error {code}"));
        };
        let issuer = || self.issuer(origin);
        self.signed_out(Fault::refusal(refusal, Route::UploadProgress, None, issuer))
    }

    /// The fault an answer other than a 200 means; a sign-in refusal drops its server's grant.
    fn refusal(&self, request: &Request, status: StatusCode, headers: &HeaderMap) -> Option<Fault> {
        let issuer = || self.issuer(&request.origin);
        Some(self.signed_out(Fault::answer(status, headers, request.route, issuer)?))
    }

    /// The fault a lane ending at `origin` means; a revoked grant is dropped and its server asks for sign-in.
    fn ending(&self, origin: &Origin, ending: LaneEnding) -> Fault {
        match ending {
            LaneEnding::Revoked => self.signed_out(Fault::SignIn(self.issuer(origin))),
            ending => Fault::Ended(ending),
        }
    }

    /// The server whose grant `origin` takes.
    fn issuer(&self, origin: &Origin) -> Origin {
        lock(&self.shared.grants).issuer(origin).clone()
    }

    /// `fault`, dropping its server's grant when it asks for sign-in.
    fn signed_out(&self, fault: Fault) -> Fault {
        if let Fault::SignIn(issuer) = &fault {
            lock(&self.shared.grants).tokens.remove(issuer);
        }
        fault
    }
}

/// Locks `mutex` past a panic elsewhere, which left its state whole.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Idle HTTP/1.1 connections kept per origin and protocol.
const IDLE: usize = 32;

/// Control connections per origin and protocol: a multiplexed one shared, idle HTTP/1.1 ones reused.
#[derive(Default)]
struct Connections {
    slots: Mutex<HashMap<(Origin, Protocol), Arc<Slot>>>,
    serial: AtomicU64,
}

#[derive(Default)]
struct Slot {
    /// The multiplexed connection, by its serial.
    shared: Mutex<Option<(u64, Conn)>>,
    /// Held across a dial that may bring the multiplexed connection, which other requests wait for.
    dialing: tokio::sync::Mutex<()>,
    /// HTTP/1.1 connections whose last answer ended.
    idle: Mutex<Vec<Conn>>,
    /// A negotiated dial found HTTP/1.1, so each request without an idle connection dials its own.
    exclusive: AtomicBool,
}

/// A connection taken for one request: the shared one's serial, and whether an earlier request used it.
struct Taken {
    conn: Conn,
    serial: Option<u64>,
    reused: bool,
}

impl Connections {
    /// The answer's head by `deadline`; a request failing unanswered on a reused connection retries once on a new one.
    async fn send(
        &self,
        client: &Client,
        via: Protocol,
        request: &Request,
        deadline: Instant,
    ) -> Result<Answer, Fault> {
        let slot = self.slot(&request.origin, via);
        match self.attempt(client, &slot, via, request, deadline, true).await {
            Err((_, true)) => self.attempt(client, &slot, via, request, deadline, false).await,
            result => result,
        }
        .map_err(|(fault, _)| fault)
    }

    /// The multiplexed connection to `origin` over `via`, dialed on the current runtime when there is none.
    async fn shared(&self, client: &Client, origin: &Origin, via: Protocol) -> Result<Conn, Fault> {
        let (slot, home) = (self.slot(origin, via), Handle::current());
        Ok(self.take(client, &slot, via, origin, true, Some(&home)).await?.conn)
    }

    /// The runtime the multiplexed QUIC connection to `origin` over `via` runs on, if there is one.
    fn home(&self, origin: &Origin, via: Protocol) -> Option<Handle> {
        let slot = self.slot(origin, via);
        let shared = lock(&slot.shared);
        shared.as_ref()?.1.home()
    }

    fn slot(&self, origin: &Origin, via: Protocol) -> Arc<Slot> {
        let mut slots = lock(&self.slots);
        slots.entry((origin.clone(), via)).or_default().clone()
    }

    /// One try; its fault says whether it failed on a reused connection before any answer.
    async fn attempt(
        &self,
        client: &Client,
        slot: &Arc<Slot>,
        via: Protocol,
        request: &Request,
        deadline: Instant,
        reuse: bool,
    ) -> Result<Answer, (Fault, bool)> {
        let head = client.head(request).map_err(|fault| (fault, false))?;
        let taken = timeout_at(deadline, self.take(client, slot, via, &request.origin, reuse, None)).await;
        let taken = taken.map_err(|_| (Fault::TimedOut("control connection"), false));
        let Taken { mut conn, serial, reused } = taken?.map_err(|fault| (fault, false))?;
        match timeout_at(deadline, conn.send(head, Payload::empty())).await {
            Ok(Ok(answer)) => Ok(slot.keep(conn, answer)),
            Ok(Err(Failed { fault, again })) => {
                if again {
                    slot.retire(serial);
                }
                Err((fault, again && reused))
            }
            Err(_) => {
                slot.retire(serial);
                Err((Fault::TimedOut("response headers"), false))
            }
        }
    }

    /// A connection for one request; a QUIC one it dials runs on `home`, else on the next pinned runtime.
    async fn take(
        &self,
        client: &Client,
        slot: &Slot,
        via: Protocol,
        origin: &Origin,
        reuse: bool,
        home: Option<&Handle>,
    ) -> Result<Taken, Fault> {
        if reuse && let Some(taken) = slot.reusable() {
            return Ok(taken);
        }
        let multiplexed = matches!(via, Protocol::Http2 | Protocol::Http3)
            || via == Protocol::Negotiated && origin.scheme == Scheme::Https;
        if !multiplexed || slot.exclusive.load(Ordering::Relaxed) {
            let conn = Conn::dial(client, origin, via, ReadBuffer::Adaptive, home).await?;
            return Ok(Taken { conn, serial: None, reused: false });
        }
        let _dialing = slot.dialing.lock().await;
        if reuse && let Some(taken) = slot.reusable() {
            return Ok(taken);
        }
        let conn = Conn::dial(client, origin, via, ReadBuffer::Adaptive, home).await?;
        let Some(shared) = conn.share() else {
            slot.exclusive.store(true, Ordering::Relaxed);
            return Ok(Taken { conn, serial: None, reused: false });
        };
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        *lock(&slot.shared) = Some((serial, shared));
        Ok(Taken { conn, serial: Some(serial), reused: false })
    }
}

impl Slot {
    /// The usable multiplexed connection, or an idle HTTP/1.1 one.
    fn reusable(&self) -> Option<Taken> {
        let shared = lock(&self.shared);
        if let Some((serial, conn)) = shared.as_ref().filter(|(_, conn)| conn.usable()) {
            return Some(Taken { conn: conn.share()?, serial: Some(*serial), reused: true });
        }
        drop(shared);
        let mut idle = lock(&self.idle);
        idle.retain(Conn::usable);
        let ready = idle.iter().position(Conn::idle)?;
        Some(Taken { conn: idle.swap_remove(ready), serial: None, reused: true })
    }

    /// Keeps an HTTP/1.1 connection for later requests once `answer`'s body ended; one sent mid-body would wait behind.
    fn keep(self: &Arc<Self>, conn: Conn, answer: Answer) -> Answer {
        if conn.share().is_some() {
            return answer;
        }
        let slot = self.clone();
        answer.map(|body| {
            body.then(move || {
                let mut idle = lock(&slot.idle);
                if idle.len() < IDLE {
                    idle.push(conn);
                }
            })
        })
    }

    /// Later requests take another connection than the shared one of `serial`; its open streams go on.
    fn retire(&self, serial: Option<u64>) {
        let mut shared = lock(&self.shared);
        if serial.is_some() && shared.as_ref().map(|(current, _)| *current) == serial {
            *shared = None;
        }
    }
}

#[cfg(test)]
mod pool_tests {
    use super::*;
    use graphite_meter_net::Pool;
    use graphite_meter_proto::route::Route;
    use http::Method;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };

    /// A request taking the connection of an answer still being read would wait behind it, as a receiver checkpoint
    /// behind the upload progress feed it shared a connection with.
    #[tokio::test]
    async fn an_http1_connection_rejoins_the_pool_once_its_answer_ended() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (end, ended) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await.unwrap());
            }
            let feed = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n11\r\n{\"type\":\"ready\"}\n\r\n";
            socket.write_all(feed).await.unwrap();
            ended.await.unwrap();
            socket.write_all(b"0\r\n\r\n").await.unwrap();
            let _ = socket.read_u8().await;
        });
        let client = Client::new(false, Arc::new(Pool::inline()));
        let origin = Origin::parse(&format!("http://{address}")).unwrap();
        let feed = Request::new(Method::GET, &origin, Route::UploadProgress);
        let mut answer = client.control(Protocol::Http1, feed).await.unwrap();
        assert!(answer.chunk().await.unwrap().is_some());
        let slot = client.connections.slot(&origin, Protocol::Http1);
        assert!(lock(&slot.idle).is_empty(), "the connection still carries its answer");
        end.send(()).unwrap();
        assert_eq!(answer.chunk().await.unwrap(), None);
        assert_eq!(lock(&slot.idle).len(), 1);
    }
}

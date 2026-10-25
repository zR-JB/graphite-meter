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

pub const UNREACHABLE: &str = "Could not reach this test server";

use crate::VERSION;
use bytes::Bytes;
use conn::{Answer, Failed};
use graphite_meter_net::{Connector, Pool, Proxy, Verify, lock};
use graphite_meter_proto::{
    discovery::{IpFamily, Probe, Protocol},
    envelope::Envelope,
    lane::LaneEnding,
    lease::{LEASE_HEADER, Lease, LeaseRequest},
    origin::{Origin, Scheme},
    refusal::Refusal,
    route::Route,
};
use http::{HeaderMap, HeaderValue, Method, StatusCode, Version, header};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    runtime::Handle,
    sync::watch,
    time::{Instant, timeout, timeout_at},
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
    leases: Mutex<Leases>,
    /// Subscribed by each unanswered lease request, so its lease is known before `end_leases` ends them.
    unanswered: watch::Sender<()>,
    runtimes: Arc<Pool>,
}

/// Each server's lease and what it holds, and the server whose lease each of its targets is presented.
#[derive(Default)]
struct Leases {
    held: HashMap<Origin, (HeaderValue, LeaseRequest)>,
    servers: HashMap<Origin, Origin>,
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

#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub origin: Origin,
    pub route: Route,
    pub query: Vec<(&'static str, String)>,
    pub body: Bytes,
}

impl Request {
    pub fn new(method: Method, origin: &Origin, route: Route) -> Self {
        Self {
            method,
            origin: origin.clone(),
            route,
            query: Vec::new(),
            body: Bytes::new(),
        }
    }
}

impl Client {
    /// A client through the environment's proxies; `insecure` skips certificate checks and never sends a grant.
    pub fn new(insecure: bool, runtimes: Arc<Pool>) -> Self {
        Self::with(
            if insecure {
                Verify::Insecure
            } else {
                Verify::trusted()
            },
            Proxy::from_env(),
            runtimes,
        )
    }

    /// A client through `proxy`, checking certificates as `verify` says; an insecure one never sends a grant.
    pub fn with(verify: Verify, proxy: Proxy, runtimes: Arc<Pool>) -> Self {
        let connector = Connector::new(proxy, verify.clone());
        let shared = Arc::new(Shared {
            connector,
            verify,
            grants: Mutex::default(),
            leases: Mutex::default(),
            unanswered: watch::Sender::new(()),
            runtimes,
        });
        Self {
            shared,
            connections: Arc::default(),
        }
    }

    /// Where latency probes run, apart from the runtimes that carry transfers.
    pub fn latency_runtime(&self) -> Handle {
        self.shared.runtimes.latency()
    }

    pub async fn json<T>(
        &self,
        via: Protocol,
        request: Request,
        decode: Decode<T>,
    ) -> Result<T, Fault> {
        Ok(self.exchange(via, request, decode).await?.1)
    }

    /// The protocol a valid probe answer from `origin` came over, and the address family the server saw.
    pub async fn probe(
        &self,
        origin: &Origin,
        via: Protocol,
    ) -> Result<(Protocol, IpFamily), Fault> {
        let request = Request::new(Method::GET, origin, Route::Probe);
        let (version, document) = self
            .exchange(via, request, |data| Ok(data.to_vec()))
            .await?;
        // The envelope comes first, so a port another release serves is named whatever the rest holds.
        let envelope = Envelope::probe(&document);
        if let Some(mismatch) =
            envelope.and_then(|envelope| envelope.mismatch(&origin.to_string(), VERSION))
        {
            return Err(Fault::Incompatible(mismatch));
        }
        let probe =
            Probe::decode(&document).map_err(|error| Fault::Malformed(error.to_string()))?;
        let protocol = match (via, version) {
            (Protocol::Negotiated, Version::HTTP_11) => Protocol::Http1,
            (Protocol::Negotiated, Version::HTTP_2) => Protocol::Http2,
            (Protocol::Negotiated, _) => {
                return Err(Fault::Malformed(
                    "probe used an unsupported HTTP protocol".into(),
                ));
            }
            (via, _) => via,
        };
        Ok((protocol, probe.client_ip_version))
    }

    /// A control request's answer, its head within the control timeout; the caller bounds its body.
    pub async fn control(&self, via: Protocol, request: Request) -> Result<Incoming, Fault> {
        let deadline = Instant::now() + CONTROL_TIMEOUT;
        let answer = self.connections.send(self, via, &request, deadline).await?;
        self.check(&request, answer)
    }

    /// A connection of its own to `origin`, dialed on the current runtime, which then runs it.
    pub async fn dial(
        &self,
        origin: &Origin,
        via: Protocol,
        buffer: ReadBuffer,
    ) -> Result<Conn, Fault> {
        Conn::dial(self, origin, via, buffer, Some(&Handle::current())).await
    }

    /// Keeps `issuer`'s grant for its and its enrolled targets' requests; a token no header can carry is dropped.
    pub fn grant(&self, issuer: &Origin, token: &str) {
        if let Ok(mut value) = HeaderValue::from_str(&format!("Bearer {token}")) {
            value.set_sensitive(true);
            lock(&self.shared.grants)
                .tokens
                .insert(issuer.clone(), value);
        }
    }

    /// Lets `server`'s grant reach its HTTPS `targets` on its host; one another server enrolled first keeps that grant.
    pub fn enroll(&self, server: &Origin, targets: &[Origin]) {
        let mut grants = lock(&self.shared.grants);
        grants.issuers.insert(server.clone(), server.clone());
        for target in targets {
            if target.scheme == Scheme::Https && target.host == server.host {
                grants
                    .issuers
                    .entry(target.clone())
                    .or_insert_with(|| server.clone());
            }
        }
    }

    /// Takes or resizes `server`'s lease for it and `targets`; answers what it holds and the upload IDs `grant` mints.
    pub async fn lease(
        &self,
        server: &Origin,
        targets: &[Origin],
        grant: LeaseRequest,
    ) -> Result<Lease, Fault> {
        {
            let mut leases = lock(&self.shared.leases);
            for target in targets.iter().chain([server]) {
                leases.servers.insert(target.clone(), server.clone());
            }
        }
        let body = serde_json::to_vec(&grant).expect("lease requests serialize");
        let request = Request {
            body: body.into(),
            ..Request::new(Method::POST, server, Route::Lease)
        };
        let (client, server) = (self.clone(), server.clone());
        let unanswered = self.shared.unanswered.subscribe();
        let asked = tokio::spawn(async move {
            let _unanswered = unanswered;
            let lease = client.json(Protocol::Negotiated, request, Lease::decode);
            lease.await.and_then(|lease| {
                let mut token = HeaderValue::from_str(&lease.token)
                    .map_err(|_| Fault::Malformed("the lease token fits no header".into()))?;
                token.set_sensitive(true);
                let held = LeaseRequest {
                    uploads: 0,
                    ..grant
                };
                lock(&client.shared.leases)
                    .held
                    .insert(server, (token, held));
                Ok(lease)
            })
        });
        asked.await.expect("lease requests do not panic")
    }

    /// A new upload ID, minted with `server`'s lease, which keeps what it holds.
    pub async fn upload_id(&self, server: &Origin) -> Result<String, Fault> {
        let held = lock(&self.shared.leases)
            .held
            .get(server)
            .map(|held| held.1);
        let grant = held.ok_or_else(|| Fault::Malformed("no lease to mint an upload ID".into()));
        let grant = grant?;
        let minted = self
            .lease(
                server,
                &[],
                LeaseRequest {
                    uploads: 1,
                    ..grant
                },
            )
            .await?;
        let id = minted.uploads.into_iter().next();
        id.ok_or_else(|| Fault::Malformed("the lease minted no upload ID".into()))
    }

    /// Ends every lease this client holds or asks for, each within a second of its answer, even once its caller left.
    pub async fn end_leases(&self) {
        let client = self.clone();
        let ended = tokio::spawn(async move {
            client.shared.unanswered.closed().await;
            let servers: Vec<_> = lock(&client.shared.leases).held.keys().cloned().collect();
            let client = &client;
            let ended = servers.into_iter().map(|server| async move {
                let request = Request::new(Method::DELETE, &server, Route::Lease);
                let deleted = client.control(Protocol::Negotiated, request);
                let _ = timeout(Duration::from_secs(1), deleted).await;
                lock(&client.shared.leases).held.remove(&server);
            });
            futures_util::future::join_all(ended).await;
        });
        let _ = ended.await;
    }

    /// Whether this client holds or asks for a lease, which `end_leases` would end.
    pub fn leased(&self) -> bool {
        self.shared.unanswered.receiver_count() > 0 || !lock(&self.shared.leases).held.is_empty()
    }

    async fn exchange<T>(
        &self,
        via: Protocol,
        request: Request,
        decode: Decode<T>,
    ) -> Result<(Version, T), Fault> {
        let deadline = Instant::now() + CONTROL_TIMEOUT;
        let exchange = async {
            let answer = self.connections.send(self, via, &request, deadline).await?;
            let version = answer.version();
            Ok((version, self.check(&request, answer)?.json(decode).await?))
        };
        let exchanged = timeout_at(deadline, exchange).await;
        exchanged.unwrap_or(Err(Fault::TimedOut("control response")))
    }

    /// The request's head with an absolute URI, uncacheable, with its server's grant over verified HTTPS and its lease.
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
        let verified = matches!(self.shared.verify, Verify::Trusted(_))
            && request.origin.scheme == Scheme::Https;
        let grants = lock(&self.shared.grants);
        let token = grants.tokens.get(grants.issuer(&request.origin));
        if let Some(token) = token.filter(|_| verified) {
            head = head.header(header::AUTHORIZATION, token.clone());
        }
        let leases = lock(&self.shared.leases);
        let server = leases.servers.get(&request.origin);
        // A lease taken with a grant signs in as it, so it goes only where the grant may.
        let held = server.filter(|server| verified || !grants.tokens.contains_key(*server));
        if let Some((token, _)) = held.and_then(|server| leases.held.get(server)) {
            head = head.header(LEASE_HEADER, token.clone());
        }
        drop((leases, grants));
        head.body(())
            .map_err(|error| Fault::Malformed(error.to_string()))
    }

    fn check(&self, request: &Request, answer: Answer) -> Result<Incoming, Fault> {
        let refused = self.refusal(request, answer.status(), answer.headers());
        refused.map_or_else(|| Ok(answer.into_body()), Err)
    }

    /// The fault an upload progress `error` record from `origin` means; a revoked grant is dropped.
    pub fn upload_error(&self, origin: &Origin, code: &str) -> Fault {
        let Some(refusal) = Refusal::find_by(Refusal::code, code) else {
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

    fn issuer(&self, origin: &Origin) -> Origin {
        lock(&self.shared.grants).issuer(origin).clone()
    }

    fn signed_out(&self, fault: Fault) -> Fault {
        if let Fault::SignIn(issuer) = &fault {
            lock(&self.shared.grants).tokens.remove(issuer);
        }
        fault
    }
}

const IDLE: usize = 32;

/// Control connections per origin and protocol: a multiplexed one shared, idle HTTP/1.1 ones reused.
#[derive(Default)]
struct Connections {
    slots: Mutex<HashMap<(Origin, Protocol), Arc<Slot>>>,
    serial: AtomicU64,
}

#[derive(Default)]
struct Slot {
    shared: Mutex<Option<(u64, Conn)>>,
    /// Held across a dial that may bring the multiplexed connection, which other requests wait for.
    dialing: tokio::sync::Mutex<()>,
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
    /// The answer's head by `deadline`; one cut unanswered on a reused connection retries once if unsent or idempotent.
    async fn send(
        &self,
        client: &Client,
        via: Protocol,
        request: &Request,
        deadline: Instant,
    ) -> Result<Answer, Fault> {
        let slot = self.slot(&request.origin, via);
        match self
            .attempt(client, &slot, via, request, deadline, true)
            .await
        {
            Err((_, true)) => {
                self.attempt(client, &slot, via, request, deadline, false)
                    .await
            }
            result => result,
        }
        .map_err(|(fault, _)| fault)
    }

    async fn shared(&self, client: &Client, origin: &Origin, via: Protocol) -> Result<Conn, Fault> {
        let (slot, home) = (self.slot(origin, via), Handle::current());
        Ok(self
            .take(client, &slot, via, origin, true, Some(&home))
            .await?
            .conn)
    }

    fn home(&self, origin: &Origin, via: Protocol) -> Option<Handle> {
        let slot = self.slot(origin, via);
        let shared = lock(&slot.shared);
        shared.as_ref()?.1.home()
    }

    fn slot(&self, origin: &Origin, via: Protocol) -> Arc<Slot> {
        let mut slots = lock(&self.slots);
        slots.entry((origin.clone(), via)).or_default().clone()
    }

    /// One try; its fault says whether the request may go again on a new connection.
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
        let taken = timeout_at(
            deadline,
            self.take(client, slot, via, &request.origin, reuse, None),
        )
        .await;
        let taken = taken.map_err(|_| (Fault::TimedOut("control connection"), false));
        let Taken {
            mut conn,
            serial,
            reused,
        } = taken?.map_err(|fault| (fault, false))?;
        let payload = match request.body.is_empty() {
            true => Payload::empty(),
            false => Payload::once(request.body.clone()),
        };
        match timeout_at(deadline, conn.send(head, payload)).await {
            Ok(Ok(answer)) => Ok(slot.keep(conn, answer)),
            Ok(Err(Failed {
                fault,
                broken,
                unsent,
            })) => {
                if broken {
                    slot.retire(serial);
                }
                let again = broken && reused && (unsent || request.method.is_idempotent());
                Err((fault, again))
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
            return Ok(Taken {
                conn,
                serial: None,
                reused: false,
            });
        }
        let _dialing = slot.dialing.lock().await;
        if reuse && let Some(taken) = slot.reusable() {
            return Ok(taken);
        }
        let conn = Conn::dial(client, origin, via, ReadBuffer::Adaptive, home).await?;
        let Some(shared) = conn.share() else {
            slot.exclusive.store(true, Ordering::Relaxed);
            return Ok(Taken {
                conn,
                serial: None,
                reused: false,
            });
        };
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        *lock(&slot.shared) = Some((serial, shared));
        Ok(Taken {
            conn,
            serial: Some(serial),
            reused: false,
        })
    }
}

impl Slot {
    fn reusable(&self) -> Option<Taken> {
        let shared = lock(&self.shared);
        if let Some((serial, conn)) = shared.as_ref().filter(|(_, conn)| conn.usable()) {
            return Some(Taken {
                conn: conn.share()?,
                serial: Some(*serial),
                reused: true,
            });
        }
        drop(shared);
        let mut idle = lock(&self.idle);
        idle.retain(Conn::usable);
        let ready = idle.iter().position(Conn::idle)?;
        Some(Taken {
            conn: idle.swap_remove(ready),
            serial: None,
            reused: true,
        })
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

    /// A request waits behind an answer still read on its connection, as a checkpoint would behind the progress feed.
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
        assert!(
            lock(&slot.idle).is_empty(),
            "the connection still carries its answer"
        );
        end.send(()).unwrap();
        assert_eq!(answer.chunk().await.unwrap(), None);
        assert_eq!(lock(&slot.idle).len(), 1);
    }
}

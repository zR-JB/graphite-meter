//! The network layer: one request API over HTTP/1.1, 2 and 3, lane groups, latency bus, failures, retry, sign-in.
pub mod approval;
mod bus;
mod conn;
mod fault;
mod lanes;
mod pool;
mod quic;
mod retry;
mod session;

pub use bus::{Bus, LatencyPath};
pub use conn::{Conn, Decode, Incoming, Payload, ReadBuffer};
pub use fault::{Class, Fault};
pub use lanes::{Carrier, GroupPlan, Lanes, ThroughputPath, Work, topology};
pub use retry::{Attempt, REDIAL_WINDOW, Retry, retrying};
pub use session::Session;

use conn::Answer;
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
    sync::{Arc, Mutex, MutexGuard, PoisonError},
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
    connections: Arc<pool::Connections>,
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
        let verify = if insecure { Verify::Insecure } else { Verify::Trusted };
        let connector = Connector::new(Proxy::from_env(), verify);
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
            token.filter(|_| self.shared.verify == Verify::Trusted && request.origin.scheme == Scheme::Https)
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

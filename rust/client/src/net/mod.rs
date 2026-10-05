//! The network layer: one request API over HTTP/1.1, HTTP/2 and HTTP/3, its failures and the retry rule.
mod conn;
mod fault;
mod pool;
mod quic;
mod retry;

pub use conn::{Decode, Incoming};
pub use fault::{Class, Fault};
pub use retry::{Attempt, REDIAL_WINDOW, Retry};

use conn::Answer;
use graphite_meter_net::{Connector, Pool, Proxy, Verify};
use graphite_meter_proto::{
    discovery::Protocol,
    origin::{Origin, Scheme},
    route::Route,
};
use http::{HeaderValue, Method, header};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};
use tokio::time::{Instant, timeout_at};

/// How long a control request may take, its answer's body included.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);

/// A path check's network state, which its run reuses: proxies, verification, grants, control connections and
/// the pinned runtimes QUIC connections run on.
#[derive(Clone)]
pub struct Client(Arc<Shared>);

struct Shared {
    connector: Connector,
    verify: Verify,
    grants: Mutex<Grants>,
    connections: pool::Connections,
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
        let connections = pool::Connections::default();
        Self(Arc::new(Shared {
            connector,
            verify,
            grants: Mutex::default(),
            connections,
            runtimes,
        }))
    }

    /// A control request's JSON answer, read by `decode` within the control timeout.
    pub async fn json<T>(&self, via: Protocol, request: Request, decode: Decode<T>) -> Result<T, Fault> {
        let deadline = Instant::now() + CONTROL_TIMEOUT;
        let answer = async { self.answer(via, &request, deadline).await?.json(decode).await };
        timeout_at(deadline, answer)
            .await
            .unwrap_or(Err(Fault::TimedOut("control response")))
    }

    /// A control request's answer, its head within the control timeout; the caller bounds its body.
    pub async fn control(&self, via: Protocol, request: Request) -> Result<Incoming, Fault> {
        self.answer(via, &request, Instant::now() + CONTROL_TIMEOUT).await
    }

    /// Keeps `issuer`'s grant for its requests and its enrolled targets'; a token no header can carry is dropped,
    /// so its server asks for sign-in again.
    pub fn grant(&self, issuer: &Origin, token: &str) {
        if let Ok(mut value) = HeaderValue::from_str(&format!("Bearer {token}")) {
            value.set_sensitive(true);
            lock(&self.0.grants).tokens.insert(issuer.clone(), value);
        }
    }

    /// Lets `server`'s grant reach those of `targets` that are HTTPS on its host; a target another server
    /// enrolled first keeps that server's grant.
    pub fn enroll(&self, server: &Origin, targets: &[Origin]) {
        let mut grants = lock(&self.0.grants);
        grants.issuers.insert(server.clone(), server.clone());
        for target in targets {
            if target.scheme == Scheme::Https && target.host == server.host {
                grants.issuers.entry(target.clone()).or_insert_with(|| server.clone());
            }
        }
    }

    async fn answer(&self, via: Protocol, request: &Request, deadline: Instant) -> Result<Incoming, Fault> {
        let answer = self.0.connections.send(self, via, request, deadline).await?;
        self.check(request, answer)
    }

    /// The request's head with an absolute URI, uncacheable, with its server's grant over verified HTTPS.
    fn head(&self, request: &Request) -> Result<http::Request<()>, Fault> {
        let mut target = format!("{}{}", request.origin, request.route.path());
        if !request.query.is_empty() {
            let query = form_urlencoded::Serializer::new(String::new())
                .extend_pairs(&request.query)
                .finish();
            target = format!("{target}?{query}");
        }
        let mut head = http::Request::builder()
            .method(request.method.clone())
            .uri(target)
            .header(header::CACHE_CONTROL, "no-store");
        let grants = lock(&self.0.grants);
        let token = grants.tokens.get(grants.issuer(&request.origin));
        if let Some(token) =
            token.filter(|_| self.0.verify == Verify::Trusted && request.origin.scheme == Scheme::Https)
        {
            head = head.header(header::AUTHORIZATION, token.clone());
        }
        head.body(()).map_err(|error| Fault::Malformed(error.to_string()))
    }

    /// The answer's body when it is a 200; a sign-in refusal drops its server's grant.
    fn check(&self, request: &Request, answer: Answer) -> Result<Incoming, Fault> {
        let issuer = || lock(&self.0.grants).issuer(&request.origin).clone();
        match Fault::answer(answer.status(), answer.headers(), request.route, issuer) {
            None => Ok(answer.into_body()),
            Some(Fault::SignIn(issuer)) => {
                lock(&self.0.grants).tokens.remove(&issuer);
                Err(Fault::SignIn(issuer))
            }
            Some(fault) => Err(fault),
        }
    }
}

/// Locks `mutex` past a panic elsewhere, which left its state whole.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

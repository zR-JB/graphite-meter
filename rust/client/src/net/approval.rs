//! Signs in natively through a PKCE approval page and polls its token exchange up to 120 s. The grant stays in memory.
use super::{
    CONTROL_TIMEOUT, Client,
    conn::{Conn, Payload, ReadBuffer},
    fault::Fault,
};
use crate::model::Failure;
use bytes::Bytes;
use graphite_meter_proto::{
    approval::{challenge, verification_code},
    discovery::Protocol,
    json,
    origin::{Origin, Scheme},
    reason::FailureReason,
    token,
};
use http::{HeaderValue, Method, StatusCode, header};
use serde_json::Value;
use std::time::Duration;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};

/// An approval waits this long for the operator.
const LIFETIME: Duration = Duration::from_mins(2);
/// Token polls are spaced this far apart.
const POLL: Duration = Duration::from_secs(1);

/// A sign-in at a server waits for the operator's approval.
pub struct Approval {
    pub origin: Origin,
    /// The approval page lives at `<origin>/auth/cli?challenge=…`.
    pub url: String,
    /// The page shows this code too.
    pub code: String,
    pub deadline: Instant,
    verifier: String,
}

/// Says why an approval gave no grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unapproved {
    Expired,
    Failed(Failure),
}

/// Says why `origin` refuses sign-in when it is not HTTPS or TLS goes unverified.
pub fn refusal(origin: &Origin, insecure: bool) -> Option<&'static str> {
    match () {
        _ if insecure => Some("Sign-in needs TLS verification; turn off Skip TLS verification (--insecure)."),
        _ if origin.scheme == Scheme::Http => Some("Sign-in needs a server address that starts with https://."),
        _ => None,
    }
}

impl Approval {
    /// Starts a fresh approval at `origin` with a verifier from 32 random bytes.
    pub fn new(origin: &Origin) -> Self {
        let mut entropy = [0; 32];
        getrandom::fill(&mut entropy).expect("the system provides randomness");
        let verifier: String = entropy.iter().map(|byte| format!("{byte:02x}")).collect();
        let challenge = challenge(&verifier);
        Self {
            origin: origin.clone(),
            url: format!("{origin}/auth/cli?challenge={challenge}"),
            code: verification_code(&challenge).expect("a S256 challenge has a code"),
            deadline: Instant::now() + LIFETIME,
            verifier,
        }
    }

    /// Returns the grant once it is approved at `client`'s server.
    pub async fn grant(&self, client: &Client) -> Result<String, Unapproved> {
        let conn = &tokio::sync::Mutex::new(None);
        self.wait(|| async move { self.poll(client, &mut *conn.lock().await).await })
            .await
    }

    /// Asks `poll` every second for the grant. Polls outlast network failures until the deadline, which reports the
    /// last of them.
    pub async fn wait<F>(&self, mut poll: impl FnMut() -> F) -> Result<String, Unapproved>
    where
        F: Future<Output = Result<Option<String>, Fault>>,
    {
        let mut unreachable = None;
        loop {
            let next = Instant::now() + POLL;
            let polled = timeout_at(self.deadline, poll()).await;
            // A poll the deadline cuts short leaves the previous outcome.
            let cut = Instant::now() >= self.deadline;
            match polled {
                Ok(Ok(Some(token))) => return Ok(token),
                Ok(Ok(None)) => unreachable = None,
                Ok(Err(fault @ Fault::Malformed(_))) => return Err(Unapproved::Failed(fault.failure())),
                Ok(Err(fault)) if !cut => unreachable = Some(fault),
                _ => {}
            }
            if cut || next >= self.deadline {
                sleep_until(self.deadline).await;
                return Err(unreachable.map_or(Unapproved::Expired, |fault| Unapproved::Failed(fault.failure())));
            }
            sleep_until(next).await;
        }
    }

    /// Runs one token exchange over `conn`, kept for the next while usable. Returns the grant, or none while pending.
    async fn poll(&self, client: &Client, conn: &mut Option<Conn>) -> Result<Option<String>, Fault> {
        let exchange = async {
            let mut ready = match conn.take().filter(Conn::usable) {
                Some(ready) => ready,
                None => Conn::dial(client, &self.origin, Protocol::Negotiated, ReadBuffer::Adaptive, None).await?,
            };
            let body = Bytes::from(format!(r#"{{"verifier":"{}"}}"#, self.verifier));
            let answer = ready.send(self.head(body.len())?, Payload::once(body)).await;
            let answer = answer.map_err(|failed| failed.fault)?;
            *conn = Some(ready);
            let status = answer.status();
            let document = answer.into_body().json(json::decode::<Value>).await;
            let grant = |document: Value| match document.get("token").and_then(Value::as_str) {
                Some(grant) if token::valid(grant) => Ok(Some(grant.to_owned())),
                _ => Err(Fault::Malformed("invalid sign-in token".into())),
            };
            match status {
                StatusCode::ACCEPTED => Ok(None),
                StatusCode::OK => grant(document?),
                status => Err(Fault::Malformed(format!("sign-in answered HTTP {}", status.as_u16()))),
            }
        };
        let exchanged = timeout(CONTROL_TIMEOUT, exchange).await;
        exchanged.unwrap_or(Err(Fault::TimedOut("sign-in answer")))
    }

    fn head(&self, length: usize) -> Result<http::Request<()>, Fault> {
        http::Request::builder()
            .method(Method::POST)
            .uri(format!("{}/auth/cli/token", self.origin))
            .header(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header(header::CONTENT_LENGTH, length)
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
            .body(())
            .map_err(|error| Fault::Malformed(error.to_string()))
    }
}

impl Unapproved {
    /// Returns the failure a view shows.
    pub fn failure(&self) -> Failure {
        match self {
            Self::Expired => Failure::new(FailureReason::SignInRequired, EXPIRED),
            Self::Failed(failure) => failure.clone(),
        }
    }
}

/// An expired approval asks this of the operator.
pub const EXPIRED: &str = "Sign-in expired";

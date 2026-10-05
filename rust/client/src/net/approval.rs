//! Native sign-in: an approval page carrying a PKCE challenge, then polling its token exchange for up to 120 s; the
//! grant stays in memory.
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
use std::{fmt::Write as _, time::Duration};
use tokio::time::{Instant, sleep_until, timeout, timeout_at};

/// How long an approval waits for the operator.
const LIFETIME: Duration = Duration::from_secs(120);
/// The spacing of token polls.
const POLL: Duration = Duration::from_secs(1);

/// A sign-in at a server waiting for the operator's approval.
pub struct Approval {
    pub origin: Origin,
    /// The approval page: `<origin>/auth/cli?challenge=…`.
    pub url: String,
    /// The code the page shows too.
    pub code: String,
    pub deadline: Instant,
    verifier: String,
}

/// Why an approval gave no grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unapproved {
    Expired,
    Failed(Failure),
}

/// Why `origin` refuses sign-in: it is not HTTPS, or TLS goes unverified.
pub fn refusal(origin: &Origin, insecure: bool) -> Option<&'static str> {
    match () {
        _ if insecure => Some("sign-in refuses skipped TLS verification (Skip TLS verify, -insecure)"),
        _ if origin.scheme == Scheme::Http => Some("authenticated operation requires an HTTPS -url"),
        _ => None,
    }
}

impl Approval {
    /// A fresh approval at `origin` with a verifier from 32 random bytes.
    pub fn new(origin: &Origin) -> Self {
        let mut entropy = [0; 32];
        getrandom::fill(&mut entropy).expect("the system provides randomness");
        let verifier = entropy.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        let challenge = challenge(&verifier);
        Self {
            origin: origin.clone(),
            url: format!("{origin}/auth/cli?challenge={challenge}"),
            code: verification_code(&challenge).expect("a S256 challenge has a code"),
            deadline: Instant::now() + LIFETIME,
            verifier,
        }
    }

    /// The grant once approved; polls outlast network failures until the deadline, which reports the last of them.
    pub async fn grant(&self, client: &Client) -> Result<String, Unapproved> {
        let (mut conn, mut unreachable) = (None, None);
        loop {
            let next = Instant::now() + POLL;
            let polled = timeout_at(self.deadline, self.poll(client, &mut conn)).await;
            // A poll the deadline cuts short leaves the previous outcome.
            let cut = Instant::now() >= self.deadline;
            match polled {
                Ok(Ok(Some(token))) => return Ok(token),
                Ok(Ok(None)) => unreachable = None,
                Ok(Err(fault @ Fault::Malformed(_))) => return Err(Unapproved::Failed(fault.failure())),
                Ok(Err(fault)) if !cut => (conn, unreachable) = (None, Some(fault)),
                _ => conn = None,
            }
            if cut || next >= self.deadline {
                sleep_until(self.deadline).await;
                return Err(unreachable.map_or(Unapproved::Expired, |fault| Unapproved::Failed(fault.failure())));
            }
            sleep_until(next).await;
        }
    }

    /// One token exchange: the grant, or none while the approval is pending.
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
                _ => Err(Fault::Malformed("invalid client approval token".into())),
            };
            match status {
                StatusCode::ACCEPTED => Ok(None),
                StatusCode::OK => grant(document?),
                status => Err(Fault::Malformed(format!("client approval returned HTTP {}", status.as_u16()))),
            }
        };
        let exchanged = timeout(CONTROL_TIMEOUT, exchange).await;
        exchanged.unwrap_or(Err(Fault::TimedOut("approval response")))
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
    /// The failure a view shows.
    pub fn failure(&self) -> Failure {
        match self {
            Self::Expired => Failure::new(FailureReason::SignInRequired, EXPIRED),
            Self::Failed(failure) => failure.clone(),
        }
    }
}

/// What an expired approval asks the operator to do.
pub const EXPIRED: &str = "Sign-in expired. Press v to request a new code.";

//! Native sign-in: a PKCE approval page, then its token exchange polled to a deadline; the grant stays in memory.
use super::{
    CONTROL_TIMEOUT, Client,
    conn::{Conn, Payload, ReadBuffer},
    fault::{Class, Fault},
};
use crate::model::Failure;
use bytes::Bytes;
use graphite_meter_proto::{
    approval::{challenge, verification_code},
    discovery::Protocol,
    json,
    origin::{Origin, Scheme},
    reason::FailureReason,
    route::AuthRoute,
    token,
};
use http::{HeaderValue, Method, StatusCode, header};
use serde_json::Value;
use std::time::Duration;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};

const LIFETIME: Duration = Duration::from_mins(2);
const POLL: Duration = Duration::from_secs(1);

pub struct Approval {
    pub origin: Origin,
    pub url: String,
    pub code: String,
    pub deadline: Instant,
    verifier: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unapproved {
    Expired,
    Failed(Failure),
}

/// Why `origin` refuses sign-in: it is not HTTPS, or TLS goes unverified.
pub fn refusal(origin: &Origin, insecure: bool) -> Option<&'static str> {
    match () {
        _ if insecure => {
            Some("Sign-in needs TLS verification; turn off Skip TLS verification (--insecure).")
        }
        _ if origin.scheme == Scheme::Http => {
            Some("Sign-in needs a server address that starts with https://.")
        }
        _ => None,
    }
}

impl Approval {
    /// A fresh approval at `origin` with a verifier from 32 random bytes.
    pub fn new(origin: &Origin) -> Self {
        let verifier: String = graphite_meter_net::random::<32>()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let challenge = challenge(&verifier);
        Self {
            origin: origin.clone(),
            url: format!("{origin}{}?challenge={challenge}", AuthRoute::Cli.path()),
            code: verification_code(&challenge).expect("a S256 challenge has a code"),
            deadline: Instant::now() + LIFETIME,
            verifier,
        }
    }

    pub async fn grant(&self, client: &Client) -> Result<String, Unapproved> {
        let conn = &tokio::sync::Mutex::new(None);
        self.wait(|| async move { self.poll(client, &mut *conn.lock().await).await })
            .await
    }

    /// The grant `poll` brings, asked every second; passing failures are retried until the deadline reports the last.
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
                Ok(Err(fault)) if fault.class() == Class::Final => {
                    return Err(Unapproved::Failed(fault.failure()));
                }
                Ok(Err(fault)) if !cut => unreachable = Some(fault),
                _ => {}
            }
            if cut || next >= self.deadline {
                sleep_until(self.deadline).await;
                return Err(unreachable.map_or(Unapproved::Expired, |fault| {
                    Unapproved::Failed(fault.failure())
                }));
            }
            sleep_until(next).await;
        }
    }

    /// One token exchange over `conn`, kept for the next while it stays usable: the grant, or none while pending.
    async fn poll(
        &self,
        client: &Client,
        conn: &mut Option<Conn>,
    ) -> Result<Option<String>, Fault> {
        let exchange = async {
            let mut ready = match conn.take().filter(Conn::usable) {
                Some(ready) => ready,
                None => {
                    Conn::dial(
                        client,
                        &self.origin,
                        Protocol::Negotiated,
                        ReadBuffer::Adaptive,
                        None,
                    )
                    .await?
                }
            };
            let body = Bytes::from(format!(r#"{{"verifier":"{}"}}"#, self.verifier));
            let answer = ready
                .send(self.head(body.len())?, Payload::once(body))
                .await;
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
                status => Err(Fault::SignInAnswer(status)),
            }
        };
        let exchanged = timeout(CONTROL_TIMEOUT, exchange).await;
        exchanged.unwrap_or(Err(Fault::TimedOut("sign-in answer")))
    }

    fn head(&self, length: usize) -> Result<http::Request<()>, Fault> {
        http::Request::builder()
            .method(Method::POST)
            .uri(format!("{}{}", self.origin, AuthRoute::CliToken.path()))
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )
            .header(header::CONTENT_LENGTH, length)
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
            .body(())
            .map_err(|error| Fault::Malformed(error.to_string()))
    }
}

impl Unapproved {
    pub fn failure(&self) -> Failure {
        match self {
            Self::Expired => Failure::new(FailureReason::SignInRequired, EXPIRED),
            Self::Failed(failure) => failure.clone(),
        }
    }
}

pub const EXPIRED: &str = "Sign-in expired";

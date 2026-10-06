//! The OIDC provider over HTTPS: discovery, signing keys, and one bounded connection per request on the main runtime.

use super::jwt::{self, Alg, Jwks, Verified};
use crate::{config::ENGINE_VERSION, lock};
use graphite_meter_net::{Connector, Proxy, Verify};
use graphite_meter_proto::{
    discovery::Protocol,
    origin::{Origin, Scheme},
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use std::{fmt::Display, future::poll_fn, pin::Pin, sync::Arc, time::Duration};
use tokio::{runtime::Handle, time::timeout};

/// Each request finishes within this.
const TIMEOUT: Duration = Duration::from_secs(10);
/// An answer's body holds at most this many bytes.
const MAX_BODY: usize = 1 << 20;

/// A 200 answer's media type, in lower case without parameters, and body.
pub(super) struct Answer {
    pub media: String,
    pub body: Vec<u8>,
}

pub(super) struct Client {
    connector: Arc<Connector>,
    runtime: Handle,
}

impl Client {
    /// A client sending from the runtime it is created on.
    pub fn new() -> Result<Self, String> {
        let runtime = Handle::try_current().map_err(|error| error.to_string())?;
        Ok(Self {
            connector: Arc::new(Connector::new(Proxy::from_env(), Verify::Trusted)),
            runtime,
        })
    }

    /// GETs `url`, or POSTs `form` URL-encoded, with header `field`; `url` is HTTPS, as config or discovery checked.
    pub async fn send(
        &self,
        url: &str,
        field: Option<(HeaderName, HeaderValue)>,
        form: Option<String>,
    ) -> Result<Answer, String> {
        let post = form.is_some();
        let mut request = Request::new(form.unwrap_or_default());
        *request.uri_mut() = url.parse().map_err(failed)?;
        if post {
            *request.method_mut() = Method::POST;
            let media = HeaderValue::from_static("application/x-www-form-urlencoded");
            request.headers_mut().insert(header::CONTENT_TYPE, media);
        }
        request.headers_mut().extend(field);
        let sending = timeout(TIMEOUT, fetch(self.connector.clone(), request));
        let sent = self.runtime.spawn(sending).await.map_err(failed)?;
        sent.map_err(|_| "OIDC request timed out".to_owned())?
    }
}

fn failed(error: impl Display) -> String {
    format!("OIDC request failed: {error}")
}

async fn fetch(connector: Arc<Connector>, mut request: Request<String>) -> Result<Answer, String> {
    let url = request.uri().to_string();
    let (origin, path) = Origin::split(&url).map_err(failed)?;
    let authority = origin.to_string().split_off("https://".len());
    let agent = format!("graphite-meter/{ENGINE_VERSION}");
    let headers = request.headers_mut();
    headers.insert(header::HOST, HeaderValue::from_str(&authority).map_err(failed)?);
    headers.insert(header::USER_AGENT, HeaderValue::from_str(&agent).map_err(failed)?);
    let path = if path.starts_with('/') { path.to_owned() } else { format!("/{path}") };
    *request.uri_mut() = path.parse().map_err(failed)?;
    let connecting = connector.connect(&origin, Some(Protocol::Http1));
    let connection = connecting.await.map_err(failed)?;
    let handshake = hyper::client::conn::http1::handshake(TokioIo::new(connection.stream));
    let (mut sender, driver) = handshake.await.map_err(failed)?;
    let exchange = async move {
        let (head, mut body) = sender.send_request(request).await.map_err(failed)?.into_parts();
        if head.status != StatusCode::OK {
            return Err(format!("OIDC provider answered {}", head.status));
        }
        let mut bytes = Vec::new();
        while let Some(frame) = poll_fn(|cx| http_body::Body::poll_frame(Pin::new(&mut body), cx)).await {
            if let Ok(data) = frame.map_err(failed)?.into_data() {
                if bytes.len() + data.len() > MAX_BODY {
                    return Err("OIDC response too large".to_owned());
                }
                bytes.extend_from_slice(&data);
            }
        }
        Ok(Answer { media: media(&head.headers), body: bytes })
    };
    tokio::join!(exchange, driver).0
}

fn media(headers: &HeaderMap) -> String {
    let value = headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok());
    let media = value.unwrap_or_default().split(';').next().unwrap_or_default();
    media.trim().to_ascii_lowercase()
}

/// The provider as discovery found it.
pub(super) struct Provider {
    authorization: String,
    /// The authorization endpoint's origin, which sign-in forms reach through the start route.
    pub origin: String,
    pub token: String,
    pub userinfo: String,
    jwks: String,
    algorithms: Vec<Alg>,
    /// The provider names itself in `iss` on every callback.
    pub issuer_parameter: bool,
    /// The signing keys; every refetch, failed or not, replaces the `Arc`.
    keys: std::sync::Mutex<Arc<Jwks>>,
    refetch: tokio::sync::Mutex<()>,
}

impl Provider {
    /// The provider `issuer` describes, which must name itself and only HTTPS endpoints.
    pub async fn discover(client: &Client, issuer: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Metadata {
            issuer: String,
            authorization_endpoint: String,
            token_endpoint: String,
            userinfo_endpoint: String,
            jwks_uri: String,
            #[serde(default)]
            id_token_signing_alg_values_supported: Vec<String>,
            #[serde(default)]
            authorization_response_iss_parameter_supported: bool,
        }
        let base = issuer.strip_suffix('/').unwrap_or(issuer);
        let url = format!("{base}/.well-known/openid-configuration");
        let answer = client.send(&url, None, None).await?;
        let metadata: Metadata = serde_json::from_slice(&answer.body).map_err(|error| error.to_string())?;
        if metadata.issuer != issuer {
            return Err(format!("provider names the issuer {:?}", metadata.issuer));
        }
        let https = |url: &str| Some(Origin::split(url).ok()?.0).filter(|origin| origin.scheme == Scheme::Https);
        let endpoints = [&metadata.token_endpoint, &metadata.userinfo_endpoint, &metadata.jwks_uri];
        let origin =
            https(&metadata.authorization_endpoint).filter(|_| endpoints.iter().all(|url| https(url).is_some()));
        let Some(origin) = origin else {
            return Err("provider metadata names a non-HTTPS endpoint".into());
        };
        let advertised = &metadata.id_token_signing_alg_values_supported;
        let algorithms = Alg::allowed(advertised);
        if algorithms.is_empty() {
            return Err(format!("provider advertises no ID token algorithm this server verifies: {advertised:?}"));
        }
        Ok(Self {
            origin: origin.to_string(),
            authorization: metadata.authorization_endpoint,
            token: metadata.token_endpoint,
            userinfo: metadata.userinfo_endpoint,
            jwks: metadata.jwks_uri,
            algorithms,
            issuer_parameter: metadata.authorization_response_iss_parameter_supported,
            keys: Default::default(),
            refetch: Default::default(),
        })
    }

    /// The provider's sign-in page for the URL-encoded `query`.
    pub fn sign_in(&self, query: &str) -> String {
        let separator = if self.authorization.contains('?') { '&' } else { '?' };
        format!("{}{separator}{query}", self.authorization)
    }

    /// Verifies `token` with the known keys, refetching them once for all callers whose keys signed nothing.
    pub async fn verify(&self, client: &Client, token: &str) -> Option<Verified> {
        let seen = lock(&self.keys).clone();
        if let Some(verified) = jwt::verify(token, &seen, &self.algorithms) {
            return Some(verified);
        }
        let _refetching = self.refetch.lock().await;
        let mut keys = lock(&self.keys).clone();
        if Arc::ptr_eq(&keys, &seen) {
            let fresh = (header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            let fetched = client.send(&self.jwks, Some(fresh), None).await.ok();
            let fetched = fetched.and_then(|answer| Jwks::parse(&answer.body));
            keys = Arc::new(fetched.unwrap_or_else(|| Jwks::clone(&keys)));
            *lock(&self.keys) = keys.clone();
        }
        jwt::verify(token, &keys, &self.algorithms)
    }
}

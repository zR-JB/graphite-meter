//! Validated discovery and origin-scoped ephemeral credentials. Redirects never carry authority.
use crate::Error;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures_util::{Stream, TryStreamExt};
use graphite_meter_core::{
    catalog::{ServerCatalog, ServerEntry},
    discovery::{Preflight, Probe, Protocol, ProtocolNegotiated},
    origin::{canonical_origin, split_url, target_origin},
    wire::decode_json,
};
use graphite_meter_net::{Proxy, connect};
use http::{
    Method, Request, StatusCode, Version,
    header::{AUTHORIZATION, CONTENT_TYPE, HOST, HeaderValue},
};
use http_body_util::{BodyExt, Empty, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    body::{Frame, Incoming},
    client::conn::{http1, http2},
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_rustls::TlsConnector;

type Result<T> = std::result::Result<T, Error>;
pub type Body = UnsyncBoxBody<Bytes, Error>;
pub type Response = http::Response<Incoming>;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_LIMIT: usize = 64 * 1024;
const IDLE_PER_ORIGIN: usize = 32;

pub fn empty() -> Body {
    Empty::new().map_err(|never| match never {}).boxed_unsync()
}
pub fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}
pub fn streaming(body: impl Stream<Item = Result<Bytes>> + Send + 'static) -> Body {
    StreamBody::new(body.map_ok(Frame::data)).boxed_unsync()
}

struct Connections {
    proxy: Proxy,
    tls: [TlsConnector; 3],
    pools: Mutex<HashMap<Key, Arc<tokio::sync::Mutex<Pool>>>>,
}
type Key = (String, Protocol);

#[derive(Default)]
struct Pool {
    h1: Vec<(tokio::time::Instant, Http1)>,
    h2: Option<http2::SendRequest<Body>>,
}
struct Http1 {
    sender: http1::SendRequest<Body>,
    absolute_form: bool,
    proxy_authorization: Option<HeaderValue>,
}
enum Sender {
    H1(Http1),
    H2(http2::SendRequest<Body>),
}

impl Connections {
    fn new(insecure: bool, proxy: Proxy) -> Result<Self> {
        let tls = |alpn: &[&[u8]]| -> Result<TlsConnector> {
            Ok(TlsConnector::from(Arc::new(crate::tls::tcp_config(
                insecure, alpn,
            )?)))
        };
        Ok(Self {
            proxy,
            tls: [
                tls(&[b"http/1.1"])?,
                tls(&[b"h2"])?,
                tls(&[b"h2", b"http/1.1"])?,
            ],
            pools: Mutex::new(HashMap::new()),
        })
    }

    async fn sender(&self, origin: &str, protocol: Protocol, pool: &mut Pool) -> Result<Sender> {
        if let Some(sender) = &pool.h2
            && !sender.is_closed()
        {
            return Ok(Sender::H2(sender.clone()));
        }
        pool.h1.retain(|(idle, connection)| {
            !connection.sender.is_closed() && idle.elapsed() < Duration::from_secs(90)
        });
        if let Some(index) = pool
            .h1
            .iter()
            .position(|(_, connection)| connection.sender.is_ready())
        {
            return Ok(Sender::H1(pool.h1.swap_remove(index).1));
        }
        let target = target_origin(origin)?.ok_or("missing origin")?;
        let tls = (target.scheme == "https").then(|| match protocol {
            Protocol::Http1 => &self.tls[0],
            Protocol::Http2 => &self.tls[1],
            _ => &self.tls[2],
        });
        let connection = connect(&self.proxy, &target, tls).await?;
        let h2 = !connection.absolute_form
            && match connection.alpn.as_deref() {
                Some(alpn) => alpn == b"h2",
                None => target.scheme == "http" && protocol == Protocol::Http2,
            };
        if protocol == Protocol::Http2 && !h2 {
            return Err("server or proxy does not support HTTP/2".into());
        }
        let io = TokioIo::new(connection.stream);
        if h2 {
            let (sender, driver) = http2::handshake(TokioExecutor::new(), io).await?;
            tokio::spawn(driver);
            pool.h2 = Some(sender.clone());
            Ok(Sender::H2(sender))
        } else {
            let (sender, driver) = http1::handshake(io).await?;
            tokio::spawn(driver);
            Ok(Sender::H1(Http1 {
                sender,
                absolute_form: connection.absolute_form,
                proxy_authorization: connection.proxy_authorization,
            }))
        }
    }

    async fn send(&self, mut request: Request<Body>, protocol: Protocol) -> Result<Response> {
        let (origin, _) = split_url(&request.uri().to_string())?;
        let origin = origin.key();
        let pool = self
            .pools
            .lock()
            .expect("connections poisoned")
            .entry((origin.clone(), protocol))
            .or_default()
            .clone();
        let sender = tokio::time::timeout(CONTROL_TIMEOUT, async {
            let mut pool = pool.lock().await;
            self.sender(&origin, protocol, &mut pool).await
        })
        .await??;
        match sender {
            Sender::H2(mut sender) => {
                sender.ready().await?;
                Ok(sender.send_request(request).await?)
            }
            Sender::H1(mut connection) => {
                let authority = request
                    .uri()
                    .authority()
                    .ok_or("missing authority")?
                    .clone();
                request
                    .headers_mut()
                    .insert(HOST, HeaderValue::from_str(authority.as_str())?);
                if connection.absolute_form {
                    if let Some(authorization) = &connection.proxy_authorization {
                        request
                            .headers_mut()
                            .insert(http::header::PROXY_AUTHORIZATION, authorization.clone());
                    }
                } else {
                    let path = request
                        .uri()
                        .path_and_query()
                        .map_or("/", |path| path.as_str())
                        .parse()?;
                    *request.uri_mut() = path;
                }
                *request.version_mut() = Version::HTTP_11;
                let response = connection.sender.send_request(request).await?;
                let mut pool = pool.lock().await;
                if pool.h1.len() < IDLE_PER_ORIGIN {
                    pool.h1.push((tokio::time::Instant::now(), connection));
                }
                Ok(response)
            }
        }
    }
}

#[derive(Clone)]
pub struct Http {
    connections: Arc<Connections>,
    insecure: bool,
    grants: Arc<Mutex<HashMap<String, Grant>>>,
    scope: Option<Arc<GrantScope>>,
}
#[derive(Clone)]
struct Grant {
    header: HeaderValue,
}
struct GrantScope {
    issuer: String,
    targets: HashSet<String>,
}
pub struct Discovery {
    pub source: String,
    pub catalog: ServerCatalog,
}
#[derive(Debug)]
pub struct AuthRequired {
    pub origin: String,
    pub login_url: String,
}
impl fmt::Display for AuthRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "authentication required at {}", self.login_url)
    }
}
impl std::error::Error for AuthRequired {}

pub(crate) fn authentication_required<'a>(
    mut error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a AuthRequired> {
    loop {
        if let Some(required) = error.downcast_ref::<AuthRequired>() {
            return Some(required);
        }
        error = error.source()?;
    }
}

/// The verifier is deliberately private and has no Debug implementation.
pub struct PendingAuthorization {
    pub browser_url: String,
    pub code: String,
    source: String,
    verifier: zeroize::Zeroizing<String>,
    token_url: String,
}

impl Http {
    pub fn new(insecure: bool) -> Result<Self> {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            return Err("install a rustls crypto provider before constructing Http".into());
        }
        Ok(Self {
            connections: Arc::new(Connections::new(insecure, Proxy::from_env())?),
            insecure,
            grants: Arc::new(Mutex::new(HashMap::new())),
            scope: None,
        })
    }
    pub async fn dial(
        &self,
        origin: &str,
        tls: Option<&TlsConnector>,
    ) -> Result<graphite_meter_net::Connection> {
        let target = target_origin(origin)?.ok_or("missing origin")?;
        Ok(connect(&self.connections.proxy, &target, tls).await?)
    }
    #[cfg(test)]
    pub(crate) fn set_proxy(&mut self, proxy: Proxy) {
        Arc::get_mut(&mut self.connections).unwrap().proxy = proxy;
    }
    pub fn authorization(&self, target: &str) -> Option<HeaderValue> {
        let origin = destination_origin(target).ok()?;
        let issuer = match &self.scope {
            Some(scope) if scope.targets.contains(&origin) => &scope.issuer,
            Some(_) => return None,
            None => &origin,
        };
        self.grants
            .lock()
            .expect("client grants poisoned")
            .get(issuer)
            .map(|grant| grant.header.clone())
    }
    pub fn builder(&self, method: Method, target: &str) -> Result<http::request::Builder> {
        destination_origin(target)?;
        let mut request = Request::builder()
            .method(method)
            .uri(target)
            .header(http::header::ACCEPT, "*/*");
        if let Some(header) = self.authorization(target) {
            request = request.header(AUTHORIZATION, header);
        }
        Ok(request)
    }
    pub async fn send(&self, request: Request<Body>, protocol: Protocol) -> Result<Response> {
        if protocol == Protocol::Http3 {
            return Err("HTTP/3 requires the native QUIC transport".into());
        }
        let target = request.uri().to_string();
        let response = self.connections.send(request, protocol).await?;
        self.check_status(&target, response.status(), response.headers())?;
        Ok(response)
    }
    pub async fn request(
        &self,
        method: Method,
        target: &str,
        protocol: Protocol,
    ) -> Result<Response> {
        let request = self.builder(method, target)?.body(empty())?;
        tokio::time::timeout(CONTROL_TIMEOUT, self.send(request, protocol)).await?
    }
    pub fn check_status(
        &self,
        target: &str,
        status: http::StatusCode,
        headers: &http::HeaderMap,
    ) -> Result<()> {
        if status == StatusCode::FORBIDDEN
            && headers
                .get("graphite-meter-auth")
                .is_some_and(|value| value == "required")
        {
            let origin = destination_origin(target)?;
            let issuer = match &self.scope {
                Some(scope) if scope.targets.contains(&origin) => &scope.issuer,
                Some(_) => {
                    return Err("authentication refusal came from an unapproved target".into());
                }
                None => &origin,
            };
            let raw = headers
                .get("graphite-meter-auth-url")
                .and_then(|value| value.to_str().ok())
                .ok_or("missing authentication URL")?;
            let login_url = validated_login(issuer, raw)?;
            let mut grants = self.grants.lock().expect("client grants poisoned");
            grants.remove(issuer);
            return Err(Box::new(AuthRequired {
                origin: issuer.clone(),
                login_url,
            }));
        }
        if !status.is_success() {
            return Err(format!("server returned HTTP {}", status.as_u16()).into());
        }
        Ok(())
    }
    pub async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        target: &str,
        protocol: Protocol,
    ) -> Result<T> {
        let bytes = self.control(method, target, protocol).await?;
        Ok(decode_json(&bytes)?)
    }
    async fn control(&self, method: Method, target: &str, protocol: Protocol) -> Result<Vec<u8>> {
        tokio::time::timeout(CONTROL_TIMEOUT, async {
            let response = self.request(method, target, protocol).await?;
            bounded_body(response).await
        })
        .await?
    }
    pub async fn discover(&self, source: &str) -> Result<Discovery> {
        let source = canonical_origin(source)?;
        let catalog: ServerCatalog = self
            .json(
                Method::GET,
                &format!("{source}/servers"),
                Protocol::Negotiated,
            )
            .await?;
        catalog.validate()?;
        let catalog = catalog.resolve(&source);
        catalog.validate()?;
        Ok(Discovery { source, catalog })
    }
    pub async fn preflight(&self, entry: &ServerEntry) -> Result<Preflight> {
        let origin = canonical_origin(&entry.url)?;
        let bytes = self
            .control(
                Method::GET,
                &format!("{origin}/preflight"),
                Protocol::Negotiated,
            )
            .await?;
        let mut preflight = Preflight::decode(&bytes)?;
        for target in &mut preflight.capabilities.throughput {
            if target.base_url == "." {
                target.base_url.clone_from(&origin);
            }
        }
        for target in &mut preflight.capabilities.latency {
            if target.base_url == "." {
                target.base_url.clone_from(&origin);
            }
        }
        entry.validate_discovery(&preflight)?;
        Ok(preflight)
    }
    pub async fn probe(&self, origin: &str, protocol: Protocol) -> Result<Probe> {
        let origin = canonical_origin(origin)?;
        let (version, bytes) = tokio::time::timeout(CONTROL_TIMEOUT, async {
            let response = self
                .request(Method::GET, &format!("{origin}/probe"), protocol)
                .await?;
            let version = response.version();
            Ok::<_, Error>((version, bounded_body(response).await?))
        })
        .await??;
        let probe = Probe::decode(&bytes)?;
        let actual = match version {
            Version::HTTP_11 => ProtocolNegotiated::Http1,
            Version::HTTP_2 => ProtocolNegotiated::Http2,
            _ => return Err("probe used an unsupported HTTP protocol".into()),
        };
        if probe.protocol_negotiated != actual {
            return Err("probe reported a different HTTP protocol than the connection".into());
        }
        Ok(probe)
    }
    /// Bind one selected server's grant to its validated HTTPS targets. The
    /// shared grant store retains issuer tokens only; overlapping target ports
    /// cannot replace another selected server's credential.
    pub fn for_server(&self, entry: &ServerEntry, preflight: &Preflight) -> Result<Self> {
        entry.validate_discovery(preflight)?;
        let issuer = canonical_origin(&entry.url)?;
        let issuer_host = target_origin(&issuer)?.ok_or("missing grant origin")?.host;
        let mut targets = HashSet::from([issuer.clone()]);
        for raw in preflight
            .capabilities
            .throughput
            .iter()
            .map(|target| &target.base_url)
            .chain(
                preflight
                    .capabilities
                    .latency
                    .iter()
                    .map(|target| &target.base_url),
            )
        {
            let origin = if raw == "." {
                issuer.clone()
            } else {
                canonical_origin(raw)?
            };
            let parsed = target_origin(&origin)?.ok_or("missing target origin")?;
            if parsed.scheme == "https" && parsed.host.eq_ignore_ascii_case(&issuer_host) {
                targets.insert(origin);
            }
        }
        let mut scoped = self.clone();
        scoped.scope = Some(Arc::new(GrantScope { issuer, targets }));
        Ok(scoped)
    }
    pub fn begin_authorization(
        &self,
        source: &str,
        auth_url: &str,
    ) -> Result<PendingAuthorization> {
        if self.insecure {
            return Err("authenticated operation refuses insecure TLS".into());
        }
        let source = canonical_origin(source)?;
        let login = validated_login(&source, auth_url)?;
        let mut entropy = [0_u8; 32];
        getrandom::fill(&mut entropy).map_err(|_| "secure randomness unavailable")?;
        let verifier = zeroize::Zeroizing::new(URL_SAFE_NO_PAD.encode(entropy));
        zeroize::Zeroize::zeroize(&mut entropy);
        let hash = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hash);
        let origin = destination_origin(&login)?;
        Ok(PendingAuthorization {
            browser_url: format!("{origin}/auth/cli?challenge={challenge}"),
            code: approval_code(&hash[..5]),
            source,
            verifier,
            token_url: format!("{origin}/auth/cli/token"),
        })
    }
    /// The caller displays browser_url/code and owns cancellation of this future.
    pub async fn poll_authorization(&self, pending: PendingAuthorization) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let body = serde_json::to_vec(
                    &serde_json::json!({"verifier": pending.verifier.as_str()}),
                )?;
                let request = Request::post(&pending.token_url)
                    .header(CONTENT_TYPE, "application/json")
                    .body(full(body))?;
                let response = tokio::time::timeout(
                    CONTROL_TIMEOUT,
                    self.connections.send(request, Protocol::Negotiated),
                )
                .await??;
                let status = response.status();
                let data = tokio::time::timeout(CONTROL_TIMEOUT, bounded_body(response)).await??;
                if status == StatusCode::OK {
                    #[derive(serde::Deserialize)]
                    struct Issued {
                        token: String,
                    }
                    let issued: Issued = decode_json(&data)?;
                    if issued.token.is_empty() || issued.token.len() > 8192 {
                        return Err("invalid client approval token".into());
                    }
                    let token = zeroize::Zeroizing::new(issued.token);
                    let mut header = HeaderValue::from_str(&format!("Bearer {}", token.as_str()))?;
                    header.set_sensitive(true);
                    let mut grants = self.grants.lock().expect("client grants poisoned");
                    grants.insert(pending.source.clone(), Grant { header });
                    return Ok(());
                }
                if status != StatusCode::ACCEPTED {
                    return Err(format!("client approval returned HTTP {}", status.as_u16()).into());
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
        .await?
    }
}

pub async fn bounded_body(response: Response) -> Result<Vec<u8>> {
    tokio::time::timeout(CONTROL_TIMEOUT, read_bounded_body(response)).await?
}
async fn read_bounded_body(response: Response) -> Result<Vec<u8>> {
    let mut body = response.into_body();
    if hyper::body::Body::size_hint(&body)
        .exact()
        .is_some_and(|length| length > CONTROL_LIMIT as u64)
    {
        return Err("control response exceeds 64 KiB".into());
    }
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        if let Ok(chunk) = frame?.into_data() {
            if chunk.len() > CONTROL_LIMIT - bytes.len() {
                return Err("control response exceeds 64 KiB".into());
            }
            bytes.extend_from_slice(&chunk);
        }
    }
    Ok(bytes)
}
fn destination_origin(raw: &str) -> Result<String> {
    Ok(canonical_origin(&split_url(raw)?.0.key())?)
}

fn validated_login(source: &str, raw: &str) -> Result<String> {
    let source = target_origin(source)?.ok_or("missing source origin")?;
    let origin = destination_origin(raw)?;
    let login = target_origin(&origin)?.ok_or("missing login origin")?;
    if source.scheme != "https"
        || login.scheme != "https"
        || !source.host.eq_ignore_ascii_case(&login.host)
        || raw != format!("{origin}/login")
    {
        return Err("server returned an invalid authentication URL".into());
    }
    Ok(raw.to_owned())
}
fn approval_code(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let bits = bytes
        .iter()
        .fold(0_u64, |bits, byte| (bits << 8) | u64::from(*byte));
    (0..8)
        .rev()
        .map(|index| ALPHABET[((bits >> (index * 5)) & 31) as usize] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphite_meter_core::discovery::{
        Capabilities, Protocol, ThroughputTarget, ThroughputTransport,
    };

    fn http(insecure: bool) -> Http {
        let _ = crate::crypto::provider().install_default();
        Http::new(insecure).unwrap()
    }

    #[tokio::test]
    async fn cleartext_proxy_requests_reuse_absolute_form_and_keep_credentials_on_proxy()
    -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for proxied in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let target = if proxied {
                "http://meter.test/probe".to_owned()
            } else {
                format!("http://{address}/probe")
            };
            let peer = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                for _ in 0..2 {
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") {
                        head.push(stream.read_u8().await.unwrap());
                    }
                    let head = String::from_utf8(head).unwrap();
                    let expected = if proxied {
                        "GET http://meter.test/probe HTTP/1.1\r\n"
                    } else {
                        "GET /probe HTTP/1.1\r\n"
                    };
                    assert!(head.starts_with(expected), "{head}");
                    assert_eq!(
                        head.contains("proxy-authorization: Basic dXNlcjpzZWNyZXQ="),
                        proxied,
                        "{head}"
                    );
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                        .await
                        .unwrap();
                }
            });
            let mut http = http(false);
            http.set_proxy(Proxy::new(&format!("http://user:secret@{address}"), "", ""));
            tokio::time::timeout(Duration::from_secs(5), async {
                for _ in 0..2 {
                    let response = http.request(Method::GET, &target, Protocol::Http1).await?;
                    assert_eq!(bounded_body(response).await?, b"ok");
                }
                peer.await?;
                Ok::<_, Error>(())
            })
            .await??;
        }
        Ok(())
    }

    #[test]
    fn request_destinations_preserve_validated_ascii_host_identity() {
        let http = http(false);
        for origin in [
            "https://xn--bcher-kva.example",
            "https://meter.example.",
            "https://127.0.0.1",
        ] {
            let request = http
                .builder(Method::GET, &format!("{origin}/probe"))
                .unwrap()
                .body(())
                .unwrap();
            assert_eq!(
                request.uri().host(),
                Some(target_origin(origin).unwrap().unwrap().host.as_str())
            );
        }
        for origin in [
            "https://1.2.3",
            "https://0x7f.1",
            "https://127.000.0.1",
            "https://127.0.0.1.",
            "https://BÜCHER.example",
            "https://xn--a.example",
            "https://meter.example:0",
        ] {
            assert!(
                http.builder(Method::GET, &format!("{origin}/probe"))
                    .is_err(),
                "accepted {origin}"
            );
        }
    }

    #[test]
    fn approval_urls_require_verified_same_host_canonical_https() {
        for raw in [
            "https://evil.example/login",
            "http://meter.example/login",
            "https://meter.example/login?redirect=evil",
            "https://meter.example/login#fragment",
            "https://user@meter.example/login",
            "https://meter.example/a/../login",
            "https://meter.example/\\login",
        ] {
            assert!(
                validated_login("https://meter.example", raw).is_err(),
                "{raw}"
            );
        }
        assert!(
            validated_login(
                "https://meter.example:443",
                "https://meter.example:8443/login"
            )
            .is_ok()
        );
        assert!(
            http(true)
                .begin_authorization("https://meter.example", "https://meter.example/login")
                .is_err()
        );
        assert_eq!(approval_code(&[0, 0, 0, 0, 0]), "AAAAAAAA");
        assert_eq!(approval_code(&[255, 255, 255, 255, 255]), "77777777");
    }

    #[test]
    fn grants_require_explicit_validated_target_enrollment() {
        let http = http(false);
        http.grants.lock().unwrap().insert(
            "https://meter.example".into(),
            Grant {
                header: HeaderValue::from_static("Bearer fixture"),
            },
        );
        assert!(
            http.authorization("https://meter.example/download")
                .is_some()
        );
        for target in [
            "https://meter.example:8443/download",
            "http://meter.example/download",
            "https://evil.example/download",
            "https://meter.example@evil.example/download",
        ] {
            assert!(http.authorization(target).is_none());
        }
        let entry = ServerEntry {
            id: "self".into(),
            url: "https://meter.example".into(),
            name: "meter".into(),
            additional_origins: vec!["https://other.example".into()],
            ..ServerEntry::default()
        };
        let preflight = Preflight::decode(br#"{"generation":"fixture","capabilities":{"throughput":[{"baseUrl":"https://meter.example:8443","transport":"fetch-stream","protocol":"http2"},{"baseUrl":"https://other.example","transport":"fetch-stream","protocol":"http1"}],"latency":[]}}"#).unwrap();
        let scoped = http.for_server(&entry, &preflight).unwrap();
        assert!(
            scoped
                .authorization("https://meter.example:8443/upload")
                .is_some()
        );
        assert!(
            scoped
                .authorization("https://other.example/upload")
                .is_none()
        );
        assert!(
            http.authorization("https://meter.example:8443/upload")
                .is_none()
        );
        let mut withdrawn = preflight.clone();
        withdrawn.capabilities.throughput.clear();
        let withdrawn = http.for_server(&entry, &withdrawn).unwrap();
        assert!(
            withdrawn
                .authorization("https://meter.example:8443/upload")
                .is_none()
        );
        assert!(
            withdrawn
                .authorization("https://meter.example/upload")
                .is_some()
        );
    }

    #[test]
    fn overlapping_selected_targets_keep_their_issuer_grants() {
        let http = http(false);
        let first = "https://meter.example:7247";
        let second = "https://meter.example:7248";
        for (issuer, token) in [(first, "Bearer first"), (second, "Bearer second")] {
            http.grants.lock().unwrap().insert(
                issuer.into(),
                Grant {
                    header: HeaderValue::from_str(token).unwrap(),
                },
            );
        }
        let entry = |id: &str, url: &str| ServerEntry {
            id: id.into(),
            url: url.into(),
            name: id.into(),
            ..ServerEntry::default()
        };
        let preflight = |target: &str| Preflight {
            server: Default::default(),
            engine_version: String::new(),
            implementation: None,
            generation: "fixture".into(),
            capabilities: Capabilities {
                upload_checkpoint: false,
                throughput: vec![ThroughputTarget {
                    base_url: target.into(),
                    transport: ThroughputTransport::FetchStream,
                    protocol: Protocol::Http2,
                }],
                latency: Vec::new(),
            },
        };
        let first_client = http
            .for_server(&entry("first", first), &preflight(second))
            .unwrap();
        let second_client = http
            .for_server(&entry("second", second), &preflight(second))
            .unwrap();
        let target = format!("{second}/upload");
        assert_eq!(first_client.authorization(&target).unwrap(), "Bearer first");
        assert_eq!(
            second_client.authorization(&target).unwrap(),
            "Bearer second"
        );
        let first_request = first_client
            .builder(Method::POST, &target)
            .unwrap()
            .body(())
            .unwrap();
        let second_request = second_client
            .builder(Method::POST, &target)
            .unwrap()
            .body(())
            .unwrap();
        assert_eq!(first_request.headers()[AUTHORIZATION], "Bearer first");
        assert_eq!(second_request.headers()[AUTHORIZATION], "Bearer second");
        assert_eq!(http.authorization(&target).unwrap(), "Bearer second");

        let mut headers = http::HeaderMap::new();
        headers.insert("graphite-meter-auth", HeaderValue::from_static("required"));
        headers.insert(
            "graphite-meter-auth-url",
            HeaderValue::from_static("https://meter.example:7247/login"),
        );
        let error = first_client
            .check_status(&target, StatusCode::FORBIDDEN, &headers)
            .unwrap_err();
        assert_eq!(
            authentication_required(error.as_ref()).unwrap().origin,
            first
        );
        assert!(first_client.authorization(&target).is_none());
        assert_eq!(
            second_client.authorization(&target).unwrap(),
            "Bearer second"
        );
    }
}

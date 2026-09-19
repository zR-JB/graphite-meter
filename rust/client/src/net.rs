//! Validated discovery and origin-scoped ephemeral credentials. Redirects never carry authority.
use crate::Error;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_core::{
    catalog::{ServerCatalog, ServerEntry},
    discovery::{Preflight, Probe, Protocol, ProtocolNegotiated},
    origin::{canonical_origin, target_origin},
    wire::decode_json,
};
use reqwest::{
    Method, Response, StatusCode,
    header::{AUTHORIZATION, HeaderValue},
};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

type Result<T> = std::result::Result<T, Error>;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_LIMIT: usize = 64 * 1024;

#[derive(Clone)]
pub struct Http {
    h1: reqwest::Client,
    h2: reqwest::Client,
    negotiated: reqwest::Client,
    insecure: bool,
    grants: Arc<Mutex<HashMap<String, Grant>>>,
}
#[derive(Clone)]
struct Grant {
    issuer: String,
    header: HeaderValue,
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
        let builder = || {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(CONTROL_TIMEOUT)
                .read_timeout(CONTROL_TIMEOUT)
                .tls_danger_accept_invalid_certs(insecure)
        };
        Ok(Self {
            h1: builder().http1_only().build()?,
            h2: builder().http2_prior_knowledge().build()?,
            negotiated: builder().build()?,
            insecure,
            grants: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub fn authorization(&self, target: &str) -> Option<HeaderValue> {
        let origin = destination_origin(target).ok()?;
        self.grants
            .lock()
            .expect("client grants poisoned")
            .get(&origin)
            .map(|grant| grant.header.clone())
    }
    pub fn builder(
        &self,
        method: Method,
        target: &str,
        protocol: Protocol,
    ) -> Result<reqwest::RequestBuilder> {
        destination_origin(target)?;
        let client = match protocol {
            Protocol::Http1 => &self.h1,
            Protocol::Http2 => &self.h2,
            Protocol::Negotiated => &self.negotiated,
            Protocol::Http3 => return Err("HTTP/3 requires the native QUIC transport".into()),
        };
        let mut request = client.request(method, target);
        if let Some(header) = self.authorization(target) {
            request = request.header(AUTHORIZATION, header);
        }
        Ok(request)
    }
    pub async fn request(
        &self,
        method: Method,
        target: &str,
        protocol: Protocol,
    ) -> Result<Response> {
        let response = tokio::time::timeout(
            CONTROL_TIMEOUT,
            self.builder(method, target, protocol)?.send(),
        )
        .await??;
        self.check_response(response)
    }
    pub fn check_response(&self, response: Response) -> Result<Response> {
        self.check_status(
            response.url().as_str(),
            response.status(),
            response.headers(),
        )?;
        Ok(response)
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
            let raw = headers
                .get("graphite-meter-auth-url")
                .and_then(|value| value.to_str().ok())
                .ok_or("missing authentication URL")?;
            let login_url = validated_login(&origin, raw)?;
            let mut grants = self.grants.lock().expect("client grants poisoned");
            if let Some(grant) = grants.get(&origin).cloned() {
                grants.retain(|_, retained| retained.issuer != grant.issuer);
            }
            return Err(Box::new(AuthRequired { origin, login_url }));
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
            reqwest::Version::HTTP_11 => ProtocolNegotiated::Http1,
            reqwest::Version::HTTP_2 => ProtocolNegotiated::Http2,
            _ => return Err("probe used an unsupported HTTP protocol".into()),
        };
        if probe.protocol_negotiated != actual {
            return Err("probe reported a different HTTP protocol than the connection".into());
        }
        Ok(probe)
    }
    /// Explicitly enroll only advertised, validated HTTPS targets on the grant's host.
    /// Additional discovery origins on a different host never receive this grant.
    pub fn approve_targets(&self, entry: &ServerEntry, preflight: &Preflight) -> Result<()> {
        entry.validate_discovery(preflight)?;
        let issuer = canonical_origin(&entry.url)?;
        let issuer_host = target_origin(&issuer)?.ok_or("missing grant origin")?.host;
        let mut grants = self.grants.lock().expect("client grants poisoned");
        let Some(grant) = grants
            .get(&issuer)
            .filter(|grant| grant.issuer == issuer)
            .cloned()
        else {
            return Ok(());
        };
        let mut targets = Vec::new();
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
                targets.push(origin);
            }
        }
        grants.retain(|origin, retained| retained.issuer != issuer || origin == &issuer);
        for target in targets {
            grants.entry(target).or_insert_with(|| grant.clone());
        }
        Ok(())
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
        rustls::crypto::ring::default_provider()
            .secure_random
            .fill(&mut entropy)
            .map_err(|_| "secure randomness unavailable")?;
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
                let response = tokio::time::timeout(
                    CONTROL_TIMEOUT,
                    self.negotiated
                        .post(&pending.token_url)
                        .json(&serde_json::json!({"verifier":pending.verifier.as_str()}))
                        .send(),
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
                    grants.retain(|_, grant| grant.issuer != pending.source);
                    grants.insert(
                        pending.source.clone(),
                        Grant {
                            issuer: pending.source,
                            header,
                        },
                    );
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
async fn read_bounded_body(mut response: Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > CONTROL_LIMIT as u64)
    {
        return Err("control response exceeds 64 KiB".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > CONTROL_LIMIT - body.len() {
            return Err("control response exceeds 64 KiB".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
fn destination_origin(raw: &str) -> Result<String> {
    if raw
        .bytes()
        .any(|byte| byte <= b' ' || byte == 127 || byte == b'\\')
    {
        return Err("invalid HTTP URL".into());
    }
    let (scheme, rest) = raw.split_once("://").ok_or("absolute HTTP URL required")?;
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .ok_or("missing URL authority")?;
    let origin = canonical_origin(&format!("{scheme}://{authority}"))?;
    let parsed = url::Url::parse(raw)?;
    if parsed.fragment().is_some() || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("HTTP URL cannot contain credentials or a fragment".into());
    }
    Ok(origin)
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

    fn http(insecure: bool) -> Http {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Http::new(insecure).unwrap()
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
                issuer: "https://meter.example".into(),
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
        http.approve_targets(&entry, &preflight).unwrap();
        assert!(
            http.authorization("https://meter.example:8443/upload")
                .is_some()
        );
        assert!(http.authorization("https://other.example/upload").is_none());
        let mut withdrawn = preflight.clone();
        withdrawn.capabilities.throughput.clear();
        http.approve_targets(&entry, &withdrawn).unwrap();
        assert!(
            http.authorization("https://meter.example:8443/upload")
                .is_none()
        );
        assert!(http.authorization("https://meter.example/upload").is_some());
    }
}

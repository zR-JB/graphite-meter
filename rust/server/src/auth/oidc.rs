use super::jwt::{self, Alg, Jwks, Reject};
use super::{
    SessionLease,
    password_login::read_secret,
    session::{random_token, token_hash},
};
use crate::config::{AuthConfig, ConfigError};
use base64::{Engine, engine::general_purpose::STANDARD};
use graphite_meter_core::origin::split_url;
use graphite_meter_net::{Proxy, connect};
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use hyper::body::Body as _;
use hyper_util::rt::TokioIo;
use rustls_platform_verifier::BuilderVerifierExt;
use serde::Deserialize;
use std::{
    collections::HashMap,
    net::IpAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex as AsyncMutex, Semaphore},
    time::Instant,
};
use tokio_rustls::TlsConnector;
use zeroize::{Zeroize, Zeroizing};

#[derive(Deserialize)]
struct Metadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    id_token_signing_alg_values_supported: Option<Vec<String>>,
    #[serde(default)]
    authorization_response_iss_parameter_supported: bool,
}

pub(super) struct Provider {
    authorization: String,
    token: String,
    userinfo: String,
    jwks_uri: String,
    algorithms: Vec<Alg>,
    keys: AsyncMutex<Arc<Jwks>>,
    issuer_parameter: bool,
    pub origin: String,
}
impl Provider {
    fn new(metadata: Metadata, keys: Jwks, issuer: &str) -> Result<Self, ConfigError> {
        if metadata.issuer != issuer {
            return Err("OIDC discovery issuer mismatch".into());
        }
        for endpoint in [
            &metadata.authorization_endpoint,
            &metadata.token_endpoint,
            &metadata.userinfo_endpoint,
            &metadata.jwks_uri,
        ] {
            valid_url(endpoint)?;
        }
        Ok(Self {
            origin: split_url(&metadata.authorization_endpoint)?.0.key(),
            algorithms: metadata
                .id_token_signing_alg_values_supported
                .as_deref()
                .map(Alg::allowed)
                .unwrap_or_else(|| vec![Alg::RS256]),
            authorization: metadata.authorization_endpoint,
            token: metadata.token_endpoint,
            userinfo: metadata.userinfo_endpoint,
            jwks_uri: metadata.jwks_uri,
            keys: AsyncMutex::new(Arc::new(keys)),
            issuer_parameter: metadata.authorization_response_iss_parameter_supported,
        })
    }
    async fn verify(&self, http: &ProviderHttp, token: &str) -> Result<jwt::Verified, Reject> {
        let seen = self.keys.lock().await.clone();
        match jwt::verify(token, &seen, &self.algorithms) {
            Err(Reject::UnknownKey) => {}
            result => return result,
        }
        let mut keys = self.keys.lock().await;
        if Arc::ptr_eq(&keys, &seen) {
            *keys = Arc::new(
                http.jwks(&self.jwks_uri)
                    .await
                    .map_err(|_| Reject::UnknownKey)?,
            );
        }
        let keys = keys.clone();
        jwt::verify(token, &keys, &self.algorithms)
    }
}
struct Transaction {
    provider: Arc<Provider>,
    browser: [u8; 32],
    nonce: Zeroizing<String>,
    verifier: Zeroizing<String>,
    deadline: Instant,
    address: IpAddr,
    pub challenge: String,
    prior: Option<SessionLease>,
}
pub(super) struct Started {
    pub url: String,
    pub browser: String,
}
pub(super) struct Identity {
    pub subject: String,
    pub name: String,
    pub challenge: String,
    pub prior: Option<SessionLease>,
}
struct Discovery {
    provider: Option<Arc<Provider>>,
    retry: Instant,
}
pub(super) struct Oidc {
    config: AuthConfig,
    secret: Zeroizing<String>,
    http: ProviderHttp,
    discovery: AsyncMutex<Discovery>,
    transactions: Mutex<HashMap<[u8; 32], Transaction>>,
    exchanges: Semaphore,
}
impl Oidc {
    pub fn new(config: &AuthConfig) -> Result<Self, ConfigError> {
        let secret = read_secret(&config.oidc_client_secret, &config.oidc_secret_file, 4096)?;
        let mut config = config.clone();
        config.oidc_client_secret.zeroize();
        config.password_hash.zeroize();
        Ok(Self {
            config,
            secret,
            http: ProviderHttp::new()?,
            discovery: AsyncMutex::new(Discovery {
                provider: None,
                retry: Instant::now(),
            }),
            transactions: Mutex::new(HashMap::new()),
            exchanges: Semaphore::new(8),
        })
    }
    pub fn name(&self) -> &str {
        &self.config.oidc_provider_name
    }
    pub async fn provider(&self) -> Result<Arc<Provider>, ConfigError> {
        let mut discovery = self
            .discovery
            .try_lock()
            .map_err(|_| "OIDC discovery in progress")?;
        if Instant::now() < discovery.retry {
            return discovery
                .provider
                .clone()
                .ok_or_else(|| "OIDC provider unavailable".into());
        }
        discovery.retry = Instant::now() + Duration::from_secs(30);
        let provider = tokio::time::timeout(Duration::from_secs(25), self.discover())
            .await
            .map_err(|_| "OIDC discovery timed out")?
            .map_err(|_| "OIDC provider discovery failed")?;
        let provider = Arc::new(provider);
        discovery.provider = Some(provider.clone());
        Ok(provider)
    }
    async fn discover(&self) -> Result<Provider, ConfigError> {
        let issuer = &self.config.oidc_issuer;
        let separator = if issuer.ends_with('/') { "" } else { "/" };
        let response = self
            .http
            .call(get(&format!(
                "{issuer}{separator}.well-known/openid-configuration"
            ))?)
            .await?;
        let metadata: Metadata = serde_json::from_slice(json(&response, &["application/json"])?)?;
        if metadata.issuer != *issuer {
            return Err("OIDC discovery issuer mismatch".into());
        }
        let keys = self.http.jwks(&metadata.jwks_uri).await?;
        Provider::new(metadata, keys, issuer)
    }
    pub async fn start(
        &self,
        address: IpAddr,
        challenge: String,
        prior: Option<SessionLease>,
    ) -> Result<Started, ConfigError> {
        let provider = self.provider().await?;
        let browser = random_token::<32>().map_err(|_| "random source unavailable")?;
        let state = random_token::<32>().map_err(|_| "random source unavailable")?;
        let nonce = Zeroizing::new(random_token::<32>().map_err(|_| "random source unavailable")?);
        let verifier =
            Zeroizing::new(random_token::<32>().map_err(|_| "random source unavailable")?);
        let pkce = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ring::digest::digest(
            &ring::digest::SHA256,
            verifier.as_bytes(),
        ));
        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.oidc_client_id)
            .append_pair("state", &state)
            .append_pair("code_challenge", &pkce)
            .append_pair("code_challenge_method", "S256")
            .append_pair("redirect_uri", &self.redirect_uri())
            .append_pair("scope", "openid profile groups")
            .append_pair("nonce", &nonce)
            .finish();
        let separator = match provider.authorization.split_once('?') {
            None => "?",
            Some((_, "")) => "",
            Some(_) => "&",
        };
        let url = format!("{}{separator}{query}", provider.authorization);
        let mut transactions = self
            .transactions
            .lock()
            .expect("OIDC transactions poisoned");
        let now = Instant::now();
        transactions.retain(|_, transaction| transaction.deadline > now);
        if transactions.len() >= 256
            || transactions
                .values()
                .filter(|tx| tx.address == address)
                .count()
                >= 8
        {
            return Err("OIDC transaction capacity reached".into());
        }
        transactions.insert(
            token_hash(&state),
            Transaction {
                provider,
                browser: token_hash(&browser),
                nonce,
                verifier,
                deadline: now + Duration::from_secs(600),
                address,
                challenge,
                prior,
            },
        );
        Ok(Started { url, browser })
    }
    fn redirect_uri(&self) -> String {
        format!("{}/auth/oidc/callback", self.config.public_url)
    }
    pub async fn finish(
        &self,
        state: &str,
        browser: &str,
        code: &str,
        issuer: Option<&str>,
    ) -> Result<Identity, ConfigError> {
        let tx = self
            .transactions
            .lock()
            .expect("OIDC transactions poisoned")
            .remove(&token_hash(state))
            .ok_or("OIDC transaction missing")?;
        if tx.deadline <= Instant::now()
            || tx.browser != token_hash(browser)
            || issuer.is_some_and(|issuer| issuer != self.config.oidc_issuer)
            || (tx.provider.issuer_parameter && issuer.is_none())
        {
            return Err("OIDC transaction rejected".into());
        }
        let _permit = self
            .exchanges
            .try_acquire()
            .map_err(|_| "OIDC exchange capacity reached")?;
        let provider = &tx.provider;
        let tokens = self
            .exchange(provider, code, &tx.verifier)
            .await
            .map_err(|_| "OIDC token exchange failed")?;
        let id_token = tokens.id_token.as_deref().ok_or("OIDC ID token missing")?;
        let verified = provider
            .verify(&self.http, id_token)
            .await
            .map_err(|_| "OIDC ID token rejected")?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let claims = jwt::id_token(
            verified,
            &jwt::Expected {
                issuer: &self.config.oidc_issuer,
                client_id: &self.config.oidc_client_id,
                nonce: &tx.nonce,
                access_token: &tokens.access_token,
                now,
            },
        )
        .map_err(|_| "OIDC ID token rejected")?;
        let info = self
            .user_info(provider, &tokens.access_token)
            .await
            .map_err(|_| "OIDC user information rejected")?;
        if info.sub != claims.subject {
            return Err("OIDC user information rejected".into());
        }
        if !info
            .groups
            .iter()
            .any(|group| self.config.oidc_allowed_groups.contains(group))
        {
            return Err("OIDC group denied".into());
        }
        let subject = claims.subject.as_str();
        if subject.is_empty() || subject.len() > 256 || subject.chars().any(char::is_control) {
            return Err("OIDC subject rejected".into());
        }
        let name = [
            info.name.as_deref(),
            info.preferred_username.as_deref(),
            claims.name.as_deref(),
            claims.preferred_username.as_deref(),
            Some(subject),
        ]
        .into_iter()
        .flatten()
        .find(|name| !name.is_empty())
        .unwrap_or("OIDC user");
        let mut name: String = name
            .chars()
            .filter(|character| !character.is_control())
            .collect();
        name.truncate(name.floor_char_boundary(256));
        if name.is_empty() {
            name.push_str("OIDC user");
        }
        Ok(Identity {
            subject: format!("oidc:{subject}"),
            name,
            challenge: tx.challenge,
            prior: tx.prior,
        })
    }
    async fn exchange(
        &self,
        provider: &Provider,
        code: &str,
        verifier: &str,
    ) -> Result<Tokens, ConfigError> {
        let encode =
            |value: &str| form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>();
        let credentials = Zeroizing::new(format!(
            "{}:{}",
            encode(&self.config.oidc_client_id),
            encode(&self.secret)
        ));
        let mut authorization = HeaderValue::from_str(&format!(
            "Basic {}",
            STANDARD.encode(credentials.as_bytes())
        ))?;
        authorization.set_sensitive(true);
        let body = form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "authorization_code")
            .append_pair("code", code)
            .append_pair("code_verifier", verifier)
            .append_pair("redirect_uri", &self.redirect_uri())
            .finish();
        let request = Request::post(&provider.token)
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::AUTHORIZATION, authorization)
            .body(body)?;
        let response = self.http.call(request).await?;
        let essence = essence(&response);
        if response.status() != StatusCode::OK
            || response.body().is_empty()
            || essence
                .as_deref()
                .is_some_and(|essence| essence != "application/json")
        {
            return Err("OIDC token endpoint rejected the exchange".into());
        }
        let tokens: Tokens = serde_json::from_slice(response.body())?;
        if !tokens.token_type.eq_ignore_ascii_case("Bearer") {
            return Err("OIDC token type unsupported".into());
        }
        Ok(tokens)
    }
    async fn user_info(
        &self,
        provider: &Provider,
        access_token: &str,
    ) -> Result<UserInfo, ConfigError> {
        let mut bearer = HeaderValue::from_str(&format!("Bearer {access_token}"))?;
        bearer.set_sensitive(true);
        let mut request = get(&provider.userinfo)?;
        request.headers_mut().insert(header::AUTHORIZATION, bearer);
        let response = self.http.call(request).await?;
        if response.status() != StatusCode::OK {
            return Err("OIDC user information unavailable".into());
        }
        match essence(&response).as_deref() {
            None | Some("application/json") => Ok(serde_json::from_slice(response.body())?),
            Some("application/jwt") => {
                let token = std::str::from_utf8(response.body())?.trim();
                let verified = provider
                    .verify(&self.http, token)
                    .await
                    .map_err(|_| "OIDC user information signature rejected")?;
                jwt::audience_and_issuer(
                    &verified.claims,
                    &self.config.oidc_issuer,
                    &self.config.oidc_client_id,
                )
                .map_err(|_| "OIDC user information claims rejected")?;
                Ok(serde_json::from_value(serde_json::Value::Object(
                    verified.claims,
                ))?)
            }
            Some(_) => Err("OIDC user information has an unexpected type".into()),
        }
    }
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    token_type: String,
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct UserInfo {
    sub: String,
    groups: Vec<String>,
    name: Option<String>,
    preferred_username: Option<String>,
}

fn valid_url(url: &str) -> Result<(), ConfigError> {
    match split_url(url) {
        Ok((origin, _)) if origin.scheme == "https" => Ok(()),
        _ => Err(
            "OIDC endpoint must be an absolute HTTPS URL without credentials or fragment".into(),
        ),
    }
}

fn get(url: &str) -> Result<Request<String>, ConfigError> {
    Ok(Request::get(url)
        .header(header::ACCEPT, "application/json")
        .body(String::new())?)
}

fn essence(response: &Response<Vec<u8>>) -> Option<String> {
    let value = response
        .headers()
        .get(header::CONTENT_TYPE)?
        .to_str()
        .unwrap_or_default();
    Some(
        value
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase(),
    )
}

fn json<'a>(response: &'a Response<Vec<u8>>, types: &[&str]) -> Result<&'a [u8], ConfigError> {
    if response.status() != StatusCode::OK
        || essence(response).is_some_and(|essence| !types.contains(&essence.as_str()))
    {
        return Err("OIDC provider returned an unexpected response".into());
    }
    Ok(response.body())
}

struct ProviderHttp {
    tls: TlsConnector,
    proxy: Proxy,
}
impl ProviderHttp {
    fn new() -> Result<Self, ConfigError> {
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(crate::crypto::provider()))
            .with_safe_default_protocol_versions()?
            .with_platform_verifier()?
            .with_no_client_auth();
        Ok(Self {
            tls: TlsConnector::from(Arc::new(tls)),
            proxy: Proxy::from_env(),
        })
    }
    async fn jwks(&self, url: &str) -> Result<Jwks, ConfigError> {
        let response = self.call(get(url)?).await?;
        let body = json(&response, &["application/json", "application/jwk-set+json"])?;
        Jwks::parse(body).map_err(|_| "OIDC key set is malformed".into())
    }
    async fn call(&self, mut request: Request<String>) -> Result<Response<Vec<u8>>, ConfigError> {
        let timeout = if request.method() == Method::GET {
            10
        } else {
            15
        };
        tokio::time::timeout(Duration::from_secs(timeout), async {
            let uri = request.uri().to_string();
            valid_url(&uri)?;
            let (origin, path) = split_url(&uri)?;
            let host = origin.key().split_off("https://".len());
            request
                .headers_mut()
                .insert(header::HOST, HeaderValue::from_str(&host)?);
            *request.uri_mut() = if path.is_empty() {
                "/".parse()?
            } else {
                path.parse()?
            };
            let connection = connect(&self.proxy, &origin, Some(&self.tls)).await?;
            let (mut sender, driver) =
                hyper::client::conn::http1::handshake(TokioIo::new(connection.stream)).await?;
            tokio::spawn(driver);
            let (parts, mut body) = sender.send_request(request).await?.into_parts();
            let mut bytes = Vec::new();
            while let Some(frame) =
                std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
            {
                if let Ok(data) = frame?.into_data() {
                    if bytes.len() + data.len() > 1024 * 1024 {
                        return Err("OIDC response too large".into());
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            Ok(Response::from_parts(parts, bytes))
        })
        .await
        .map_err(|_| "OIDC request timed out")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthMode;

    async fn ready() -> Oidc {
        let oidc = Oidc::new(&AuthConfig {
            mode: AuthMode::Oidc,
            public_url: "https://meter.example".into(),
            oidc_issuer: "https://identity.example".into(),
            oidc_client_id: "meter".into(),
            oidc_client_secret: "secret".into(),
            oidc_allowed_groups: vec!["operators".into()],
            ..AuthConfig::default()
        })
        .unwrap();
        let metadata = serde_json::from_value(serde_json::json!({
            "issuer": "https://identity.example",
            "authorization_endpoint": "https://identity.example/authorize",
            "token_endpoint": "https://identity.example/token",
            "userinfo_endpoint": "https://identity.example/userinfo",
            "jwks_uri": "https://identity.example/jwks",
            "id_token_signing_alg_values_supported": ["RS256"],
            "authorization_response_iss_parameter_supported": true
        }))
        .unwrap();
        let keys = Jwks::parse(br#"{"keys":[]}"#).unwrap();
        let provider = Provider::new(metadata, keys, "https://identity.example").unwrap();
        *oidc.discovery.lock().await = Discovery {
            provider: Some(Arc::new(provider)),
            retry: Instant::now() + Duration::from_secs(30),
        };
        oidc
    }

    pub(super) fn query_fields(url: &str) -> HashMap<String, String> {
        form_urlencoded::parse(url.split_once('?').unwrap().1.as_bytes())
            .into_owned()
            .collect()
    }

    #[tokio::test]
    async fn authorization_is_pkce_bound_bounded_and_consumed_before_browser_validation() {
        let oidc = ready().await;
        let address = "192.0.2.1".parse().unwrap();
        let started = oidc.start(address, String::new(), None).await.unwrap();
        let fields = query_fields(&started.url);
        assert_eq!(fields["code_challenge_method"], "S256");
        assert_eq!(fields["response_type"], "code");
        assert_eq!(
            fields["redirect_uri"],
            "https://meter.example/auth/oidc/callback"
        );
        assert!(
            oidc.finish(
                &fields["state"],
                "wrong-browser",
                "code",
                Some("https://identity.example")
            )
            .await
            .is_err()
        );
        assert!(oidc.transactions.lock().unwrap().is_empty());
        assert!(
            oidc.finish(
                &fields["state"],
                &started.browser,
                "code",
                Some("https://identity.example")
            )
            .await
            .is_err()
        );
        for _ in 0..8 {
            oidc.start(address, String::new(), None).await.unwrap();
        }
        assert!(oidc.start(address, String::new(), None).await.is_err());
        assert_eq!(oidc.transactions.lock().unwrap().len(), 8);
    }

    #[tokio::test]
    async fn mismatched_response_issuer_cannot_redeem_a_code() {
        let oidc = ready().await;
        let started = oidc
            .start("192.0.2.2".parse().unwrap(), String::new(), None)
            .await
            .unwrap();
        let state = query_fields(&started.url)["state"].clone();
        assert!(
            oidc.finish(
                &state,
                &started.browser,
                "code",
                Some("https://other.example")
            )
            .await
            .is_err()
        );
        assert!(oidc.transactions.lock().unwrap().is_empty());
    }

    #[test]
    fn provider_endpoints_require_https_without_embedded_credentials() {
        for endpoint in [
            "http://identity.example/token",
            "https://user@identity.example/token",
            "https://identity.example/token#fragment",
        ] {
            assert!(valid_url(endpoint).is_err());
        }
        assert!(valid_url("https://identity.example/token").is_ok());
    }
}

#[cfg(test)]
#[path = "oidc_tests.rs"]
mod provider_tests;

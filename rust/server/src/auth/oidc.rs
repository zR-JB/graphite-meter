use super::jwt::{self, Alg, Jwks, Reject};
use super::{
    SessionLease,
    password_login::read_secret,
    reason::Reason,
    session::{random_token, token_hash},
};
use crate::config::{AuthConfig, ConfigError};
use crate::sync::lock;
use base64::{Engine, engine::general_purpose::STANDARD};
use graphite_meter_core::origin::split_url;
use graphite_meter_net::{Proxy, connect};
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header};
use hyper::body::Body as _;
use hyper_util::rt::TokioIo;
use rustls_platform_verifier::BuilderVerifierExt;
use serde::Deserialize;
use std::{
    collections::HashMap,
    net::IpAddr,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex as AsyncMutex, Semaphore},
    time::Instant,
};
use tokio_rustls::TlsConnector;
use zeroize::{Zeroize, Zeroizing};

const MAX_TRANSACTIONS: usize = 16384;
/// Go's oidcTransactionLifetime, which the transaction cookie's Max-Age also carries.
pub(super) const TRANSACTION_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// Go bounds a callback's provider calls at 15 s.
const CALLBACK_DEADLINE: Duration = Duration::from_secs(15);
/// Go's provider client timeout, for each call.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(10);
/// Discovery retries back off from one second, doubling to this.
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Concurrent token exchanges; a callback past them waits within its deadline.
const MAX_EXCHANGES: usize = 8;

#[derive(Deserialize)]
struct Metadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    jwks_uri: String,
    #[serde(default, deserialize_with = "jwt::nullable")]
    id_token_signing_alg_values_supported: Vec<String>,
    /// Go reads this member apart from the rest and takes a mistyped value as false.
    #[serde(default)]
    authorization_response_iss_parameter_supported: serde_json::Value,
}

pub(super) struct Provider {
    authorization: String,
    token: String,
    userinfo: String,
    jwks_uri: String,
    algorithms: Vec<Alg>,
    keys: AsyncMutex<Arc<Jwks>>,
    issuer_parameter: bool,
    /// Sign-in page headers whose form-action admits the authorization origin.
    pub page_headers: HeaderMap,
}
impl Provider {
    fn new(metadata: Metadata, issuer: &str) -> Result<Self, ConfigError> {
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
        // Checked here rather than on every sign-in page: a browser can post only to a canonical origin.
        let origin = split_url(&metadata.authorization_endpoint)?.0.key();
        let page_headers = super::pages::security_headers(Some(&origin))
            .map_err(|_| format!("OIDC authorization endpoint origin {origin:?} is not a canonical HTTPS origin"))?;
        Ok(Self {
            page_headers,
            algorithms: Alg::allowed(&metadata.id_token_signing_alg_values_supported),
            authorization: metadata.authorization_endpoint,
            token: metadata.token_endpoint,
            userinfo: metadata.userinfo_endpoint,
            jwks_uri: metadata.jwks_uri,
            // Fetched with the first token, as by go-oidc.
            keys: AsyncMutex::new(Arc::new(Jwks::parse(br#"{"keys":[]}"#).expect("empty key set"))),
            issuer_parameter: metadata.authorization_response_iss_parameter_supported == true,
        })
    }
    async fn verify(&self, http: &ProviderHttp, token: &str) -> Result<jwt::Verified, Reject> {
        let seen = self.keys.lock().await.clone();
        match jwt::verify(token, &seen, &self.algorithms) {
            Err(Reject::UnknownKey | Reject::Signature) => {}
            result => return result,
        }
        let mut keys = self.keys.lock().await;
        if Arc::ptr_eq(&keys, &seen) {
            *keys = Arc::new(http.jwks(&self.jwks_uri).await.map_err(|_| Reject::UnknownKey)?);
        }
        let keys = keys.clone();
        jwt::verify(token, &keys, &self.algorithms)
    }
}
pub(super) struct Transaction {
    provider: Arc<Provider>,
    browser: [u8; 32],
    nonce: Zeroizing<String>,
    verifier: Zeroizing<String>,
    deadline: Instant,
    client_keys: Vec<String>,
    pub challenge: String,
    pub prior: Option<SessionLease>,
}
pub(super) struct Started {
    pub url: String,
    pub browser: String,
    pub provider: Arc<Provider>,
}
pub(super) struct Identity {
    pub subject: String,
    pub name: String,
}

pub(super) struct Oidc {
    config: AuthConfig,
    log: Arc<super::logging::SecurityLog>,
    secret: Zeroizing<String>,
    http: ProviderHttp,
    provider: OnceLock<Arc<Provider>>,
    transactions: Mutex<HashMap<[u8; 32], Transaction>>,
    exchanges: Semaphore,
}
impl Oidc {
    pub fn new(config: &AuthConfig, log: Arc<super::logging::SecurityLog>) -> Result<Self, ConfigError> {
        let secret = read_secret(
            "OIDC client secret",
            &config.oidc_client_secret,
            &config.oidc_secret_file,
            16 * 1024,
        )?;
        let mut config = config.clone();
        config.oidc_client_secret.zeroize();
        config.password_hash.zeroize();
        Ok(Self {
            config,
            log,
            secret,
            http: ProviderHttp::new()?,
            provider: OnceLock::new(),
            transactions: Mutex::new(HashMap::new()),
            exchanges: Semaphore::new(MAX_EXCHANGES),
        })
    }
    pub fn name(&self) -> &str {
        &self.config.oidc_provider_name
    }
    pub fn ready(&self) -> Option<&Arc<Provider>> {
        self.provider.get()
    }
    pub async fn discover(&self) -> Result<(), ConfigError> {
        let provider = self.fetch_provider().await?;
        if self.provider.set(Arc::new(provider)).is_ok() {
            crate::log!("[gm:auth] OIDC provider ready");
        }
        Ok(())
    }
    pub async fn retry_discovery(&self) {
        let mut failures = 0u32;
        while self.provider.get().is_none() {
            let Err(error) = self.discover().await else {
                return;
            };
            if failures == 0 {
                crate::log!("[gm:auth] OIDC provider unavailable; local password remains available");
            } else {
                crate::log!("[gm:auth] OIDC provider retrying");
            }
            self.log.debug(format_args!("OIDC discovery failed: {error}"));
            tokio::time::sleep(Duration::from_secs(1 << failures.min(6)).min(RETRY_MAX)).await;
            failures = failures.saturating_add(1);
        }
    }
    async fn fetch_provider(&self) -> Result<Provider, ConfigError> {
        let issuer = &self.config.oidc_issuer;
        let separator = if issuer.ends_with('/') { "" } else { "/" };
        let response = self
            .http
            .call(get(&format!("{issuer}{separator}.well-known/openid-configuration"))?)
            .await?;
        let metadata: Metadata = serde_json::from_slice(ok(&response)?)?;
        Provider::new(metadata, issuer)
    }
    pub async fn start(
        &self,
        address: IpAddr,
        challenge: String,
        prior: Option<SessionLease>,
    ) -> Result<Started, Reason> {
        let provider = self.ready().ok_or(Reason::ProviderNotReady)?.clone();
        let random = || random_token::<32>().map_err(|_| Reason::TransactionCapacity);
        let (browser, state) = (random()?, random()?);
        let (nonce, verifier) = (Zeroizing::new(random()?), Zeroizing::new(random()?));
        let pkce = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()));
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
        let mut transactions = lock(&self.transactions);
        let now = Instant::now();
        transactions.retain(|_, transaction| transaction.deadline > now);
        let client_keys = crate::client_address::client_keys(address);
        if transactions.len() >= MAX_TRANSACTIONS
            || crate::client_address::share_full(&client_keys, 8, |key| {
                transactions
                    .values()
                    .filter(|tx| tx.client_keys.iter().any(|held| held == key))
                    .count()
            })
        {
            if transactions.len() >= MAX_TRANSACTIONS {
                self.log.ceiling(super::logging::Ceiling::OidcTransaction);
            }
            return Err(Reason::TransactionCapacity);
        }
        transactions.insert(
            token_hash(&state),
            Transaction {
                provider: provider.clone(),
                browser: token_hash(&browser),
                nonce,
                verifier,
                deadline: now + TRANSACTION_LIFETIME,
                client_keys,
                challenge,
                prior,
            },
        );
        Ok(Started { url, browser, provider })
    }
    fn redirect_uri(&self) -> String {
        format!("{}/auth/oidc/callback", self.config.public_url)
    }
    pub fn take(&self, state: &str, browser: &str, issuer: Option<&str>) -> Result<Transaction, (Reason, String)> {
        let tx = lock(&self.transactions)
            .remove(&token_hash(state))
            .ok_or((Reason::TransactionReplay, String::new()))?;
        if tx.deadline <= Instant::now() || tx.browser != token_hash(browser) {
            return Err((Reason::TransactionReplay, tx.challenge));
        }
        // As in Go, an empty iss is absent: refused only when the provider advertises it.
        let issuer = issuer.filter(|issuer| !issuer.is_empty());
        if issuer.is_some_and(|issuer| issuer != self.config.oidc_issuer)
            || (tx.provider.issuer_parameter && issuer.is_none())
        {
            return Err((Reason::ResponseIssuer, tx.challenge));
        }
        Ok(tx)
    }

    pub async fn complete(&self, tx: &Transaction, code: &str) -> Result<Identity, Reason> {
        let deadline = Instant::now() + CALLBACK_DEADLINE;
        let _permit = within(deadline, Reason::TokenExchange, self.exchanges.acquire()).await?;
        let provider = &tx.provider;
        let tokens = within(
            deadline,
            Reason::TokenExchange,
            self.exchange(provider, code, &tx.verifier),
        )
        .await?;
        let id_token = tokens
            .id_token
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .ok_or(Reason::MissingIdToken)?;
        let verified = within(
            deadline,
            Reason::IdTokenVerification,
            provider.verify(&self.http, id_token),
        )
        .await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Reason::IdTokenVerification)?
            .as_secs();
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
        .map_err(|reject| match reject {
            Reject::Nonce => Reason::IdTokenClaimsOrNonce,
            Reject::AccessTokenHash => Reason::AccessTokenHash,
            _ => Reason::IdTokenVerification,
        })?;
        let info = within(
            deadline,
            Reason::UserInfoOrSubject,
            self.user_info(provider, &tokens.access_token),
        )
        .await?;
        if info.get("sub").and_then(serde_json::Value::as_str) != Some(&claims.subject) {
            return Err(Reason::UserInfoOrSubject);
        }
        let info: UserInfo = serde_json::from_value(info).map_err(|_| Reason::UserInfoClaims)?;
        if !info
            .groups
            .iter()
            .any(|group| self.config.oidc_allowed_groups.contains(group))
        {
            return Err(Reason::GroupDenied);
        }
        let subject = claims.subject.as_str();
        if subject.is_empty()
            || subject.len() > 256
            || !subject.chars().all(graphite_meter_core::text::display_character)
        {
            return Err(Reason::InvalidSubject);
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
        .unwrap_or(subject);
        // As Go's safeDisplayName.
        let cleaned = graphite_meter_core::text::clean(name, 64);
        let name = match cleaned.trim() {
            "" => "OIDC user",
            name => name,
        };
        Ok(Identity {
            subject: format!("oidc:{subject}"),
            name: name.to_owned(),
        })
    }
    async fn exchange(&self, provider: &Provider, code: &str, verifier: &str) -> Result<Tokens, ConfigError> {
        // x/oauth2 applies Go's url.QueryEscape, which keeps '~' and escapes '*' unlike a form encoder.
        let encode = |value: &str| {
            form_urlencoded::byte_serialize(value.as_bytes())
                .collect::<String>()
                .replace("%7E", "~")
                .replace('*', "%2A")
        };
        let credentials = Zeroizing::new(format!(
            "{}:{}",
            encode(&self.config.oidc_client_id),
            encode(&self.secret)
        ));
        let mut authorization = HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(credentials.as_bytes())))?;
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
        if !response.status().is_success() {
            return Err("OIDC token endpoint rejected the exchange".into());
        }
        // As golang.org/x/oauth2 reads it: a form for form and text types, JSON otherwise.
        let tokens: Tokens = match essence(&response).as_deref() {
            Some("application/x-www-form-urlencoded" | "text/plain") => {
                let mut fields = HashMap::new();
                for (key, value) in form_urlencoded::parse(response.body()) {
                    fields.entry(key).or_insert(value);
                }
                serde_json::from_value(serde_json::to_value(fields)?)?
            }
            _ => serde_json::from_slice(response.body())?,
        };
        if !tokens.error.is_empty() || tokens.access_token.is_empty() {
            return Err("OIDC token endpoint rejected the exchange".into());
        }
        Ok(tokens)
    }
    async fn user_info(&self, provider: &Provider, access_token: &str) -> Result<serde_json::Value, ConfigError> {
        let mut bearer = HeaderValue::from_str(&format!("Bearer {access_token}"))?;
        bearer.set_sensitive(true);
        let mut request = get(&provider.userinfo)?;
        request.headers_mut().insert(header::AUTHORIZATION, bearer);
        let response = self.http.call(request).await?;
        if response.status() != StatusCode::OK {
            return Err("OIDC user information unavailable".into());
        }
        if essence(&response).as_deref() != Some("application/jwt") {
            return Ok(serde_json::from_slice(response.body())?);
        }
        let token = std::str::from_utf8(response.body())?.trim();
        let verified = provider
            .verify(&self.http, token)
            .await
            .map_err(|_| "OIDC user information signature rejected")?;
        jwt::audience_and_issuer(&verified.claims, &self.config.oidc_issuer, &self.config.oidc_client_id)
            .map_err(|_| "OIDC user information claims rejected")?;
        Ok(serde_json::Value::Object(verified.claims))
    }
}

#[derive(Deserialize)]
struct Tokens {
    #[serde(default, deserialize_with = "jwt::nullable")]
    access_token: String,
    #[serde(default, deserialize_with = "jwt::nullable")]
    error: String,
    id_token: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct UserInfo {
    #[serde(default)]
    groups: Vec<String>,
    name: Option<String>,
    preferred_username: Option<String>,
}

/// A step that fails or outlasts the callback's deadline refuses it with `reason`, as Go's context does.
async fn within<T, E>(
    deadline: Instant,
    reason: Reason,
    step: impl Future<Output = Result<T, E>>,
) -> Result<T, Reason> {
    tokio::time::timeout_at(deadline, step)
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or(reason)
}

fn valid_url(url: &str) -> Result<(), ConfigError> {
    match split_url(url) {
        Ok((origin, _)) if origin.scheme == "https" => Ok(()),
        _ => Err("OIDC endpoint must be an absolute HTTPS URL without credentials or fragment".into()),
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
    Some(value.split(';').next().unwrap_or_default().trim().to_ascii_lowercase())
}

/// go-oidc reads discovery and key sets whatever their content type.
fn ok(response: &Response<Vec<u8>>) -> Result<&[u8], ConfigError> {
    if response.status() != StatusCode::OK {
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
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
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
        Jwks::parse(ok(&response)?).map_err(|_| "OIDC key set is malformed".into())
    }
    async fn call(&self, mut request: Request<String>) -> Result<Response<Vec<u8>>, ConfigError> {
        tokio::time::timeout(PROVIDER_TIMEOUT, async {
            let uri = request.uri().to_string();
            valid_url(&uri)?;
            let (origin, path) = split_url(&uri)?;
            let host = origin.key().split_off("https://".len());
            request
                .headers_mut()
                .insert(header::HOST, HeaderValue::from_str(&host)?);
            *request.uri_mut() = if path.is_empty() { "/".parse()? } else { path.parse()? };
            let connection = connect(&self.proxy, &origin, Some(&self.tls)).await?;
            let (mut sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(connection.stream)).await?;
            let exchange = async move {
                let (parts, mut body) = sender.send_request(request).await?.into_parts();
                let mut bytes = Vec::new();
                while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
                    if let Ok(data) = frame?.into_data() {
                        if bytes.len() + data.len() > 1024 * 1024 {
                            return Err("OIDC response too large".into());
                        }
                        bytes.extend_from_slice(&data);
                    }
                }
                Ok(Response::from_parts(parts, bytes))
            };
            tokio::join!(exchange, driver).0
        })
        .await
        .map_err(|_| "OIDC request timed out")?
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::config::AuthMode;

    pub(in crate::auth) fn ready() -> Oidc {
        ready_with(true)
    }

    fn ready_with(issuer_parameter: bool) -> Oidc {
        let oidc = discovered("https://identity.example/authorize", issuer_parameter);
        assert!(oidc.ready().is_some());
        oidc
    }

    /// A client that discovered metadata naming `authorization_endpoint`, ready if discovery accepted it.
    pub(in crate::auth) fn discovered(authorization_endpoint: &str, issuer_parameter: bool) -> Oidc {
        let oidc = Oidc::new(
            &AuthConfig {
                mode: AuthMode::Oidc,
                public_url: "https://meter.example".into(),
                oidc_issuer: "https://identity.example".into(),
                oidc_client_id: "meter".into(),
                oidc_client_secret: "secret".into(),
                oidc_allowed_groups: vec!["operators".into()],
                ..AuthConfig::default()
            },
            Arc::new(crate::auth::logging::SecurityLog::default()),
        )
        .unwrap();
        let metadata = serde_json::from_value(serde_json::json!({
            "issuer": "https://identity.example",
            "authorization_endpoint": authorization_endpoint,
            "token_endpoint": "https://identity.example/token",
            "userinfo_endpoint": "https://identity.example/userinfo",
            "jwks_uri": "https://identity.example/jwks",
            "id_token_signing_alg_values_supported": ["RS256"],
            "authorization_response_iss_parameter_supported": issuer_parameter
        }))
        .unwrap();
        if let Ok(provider) = Provider::new(metadata, "https://identity.example") {
            assert!(oidc.provider.set(Arc::new(provider)).is_ok());
        }
        oidc
    }

    pub(in crate::auth) fn query_fields(url: &str) -> HashMap<String, String> {
        form_urlencoded::parse(url.split_once('?').unwrap().1.as_bytes())
            .into_owned()
            .collect()
    }

    #[tokio::test]
    async fn authorization_is_pkce_bound_bounded_and_consumed_before_browser_validation() {
        let oidc = ready();
        let address = "192.0.2.1".parse().unwrap();
        let started = oidc.start(address, String::new(), None).await.unwrap();
        let fields = query_fields(&started.url);
        assert_eq!(fields["code_challenge_method"], "S256");
        assert_eq!(fields["response_type"], "code");
        assert_eq!(fields["redirect_uri"], "https://meter.example/auth/oidc/callback");
        let issuer = Some("https://identity.example");
        let refused = oidc.take(&fields["state"], "wrong-browser", issuer).err();
        assert_eq!(refused, Some((Reason::TransactionReplay, String::new())));
        assert!(oidc.transactions.lock().unwrap().is_empty());
        assert!(oidc.take(&fields["state"], &started.browser, issuer).is_err());
        for _ in 0..8 {
            oidc.start(address, String::new(), None).await.unwrap();
        }
        assert!(oidc.start(address, String::new(), None).await.is_err());
        assert_eq!(oidc.transactions.lock().unwrap().len(), 8);
    }

    #[tokio::test]
    async fn transactions_charge_wider_ipv6_shares_before_global_capacity() {
        let oidc = ready();
        for subnet in 0..2 {
            for host in 1..=8 {
                let address = format!("2001:db8:1:{subnet:x}::{host}").parse().unwrap();
                oidc.start(address, String::new(), None).await.unwrap();
            }
        }
        assert!(
            oidc.start("2001:db8:1:2::1".parse().unwrap(), String::new(), None)
                .await
                .is_err()
        );
        assert!(
            oidc.start("2001:db8:2::1".parse().unwrap(), String::new(), None)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn mismatched_response_issuer_cannot_redeem_a_code() {
        let oidc = ready();
        let started = oidc
            .start("192.0.2.2".parse().unwrap(), "challenge".into(), None)
            .await
            .unwrap();
        let state = query_fields(&started.url)["state"].clone();
        let refused = oidc.take(&state, &started.browser, Some("https://other.example")).err();
        assert_eq!(refused, Some((Reason::ResponseIssuer, "challenge".into())));
        assert!(oidc.transactions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_empty_response_issuer_is_absent_like_go() {
        for advertised in [false, true] {
            let oidc = ready_with(advertised);
            let started = oidc
                .start("192.0.2.3".parse().unwrap(), "challenge".into(), None)
                .await
                .unwrap();
            let state = query_fields(&started.url)["state"].clone();
            let taken = oidc
                .take(&state, &started.browser, Some(""))
                .map(|_| ())
                .map_err(|(reason, _)| reason);
            assert_eq!(
                taken,
                if advertised {
                    Err(Reason::ResponseIssuer)
                } else {
                    Ok(())
                }
            );
        }
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

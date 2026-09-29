use super::jwt::{self, Alg, Jwks, Reject, go_json, go_str};
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
use serde_json::Value;
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
    /// From go-oidc's providerJSON, whose members Go's decoder matches in any case, the last one winning, and where
    /// one of another type refuses the document.
    fn new(metadata: &Members, issuer: &str) -> Result<Self, ConfigError> {
        let malformed = "OIDC discovery document is malformed";
        let text = |name| metadata.text(name).ok_or(malformed);
        let algorithms = metadata
            .strings("id_token_signing_alg_values_supported")
            .ok_or(malformed)?;
        text("device_authorization_endpoint")?;
        if text("issuer")? != issuer {
            return Err("OIDC discovery issuer mismatch".into());
        }
        // Go's client leaves out a fragment. One on the authorization endpoint stays refused: Go appends the
        // sign-in query to it, where no provider reads it.
        let fetched = |name| text(name).map(|url| url.split('#').next().unwrap_or_default().to_owned());
        let (token, userinfo, jwks_uri) = (
            fetched("token_endpoint")?,
            fetched("userinfo_endpoint")?,
            fetched("jwks_uri")?,
        );
        let authorization = text("authorization_endpoint")?.to_owned();
        for endpoint in [&authorization, &token, &userinfo, &jwks_uri] {
            valid_url(endpoint)?;
        }
        // Checked here rather than on every sign-in page: a browser can post only to a canonical origin.
        let origin = split_url(&authorization)?.0.key();
        let page_headers = super::pages::security_headers(Some(&origin))
            .ok_or_else(|| format!("OIDC authorization endpoint origin {origin:?} is not a canonical HTTPS origin"))?;
        // Go reads this member apart from the rest, where one of another type leaves it as it was.
        let issuer_parameter = metadata.named("authorization_response_iss_parameter_supported");
        Ok(Self {
            page_headers,
            algorithms: Alg::allowed(&algorithms),
            authorization,
            token,
            userinfo,
            jwks_uri,
            // Fetched with the first token, as by go-oidc.
            keys: AsyncMutex::new(Arc::new(Jwks::parse(br#"{"keys":[]}"#).expect("empty key set"))),
            issuer_parameter: issuer_parameter.fold(false, |on, value| value.as_bool().unwrap_or(on)),
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
        Provider::new(&go_json(ok(&response)?)?, issuer)
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
        // Sorted by name, as x/oauth2's AuthCodeURL encodes them.
        let query = encode(&[
            ("client_id", &self.config.oidc_client_id),
            ("code_challenge", &pkce),
            ("code_challenge_method", "S256"),
            ("nonce", &nonce),
            ("redirect_uri", &self.redirect_uri()),
            ("response_type", "code"),
            ("scope", "openid profile groups"),
            ("state", &state),
        ]);
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
        // go-oidc's UserInfo reads these members and refuses a mistyped one; email_verified may be a string.
        let flag = |value: &Value| value.is_boolean() || matches!(value.as_str(), Some("true" | "false"));
        if info.text("sub") != Some(&claims.subject)
            || info.text("profile").is_none()
            || info.text("email").is_none()
            || !info.named("email_verified").all(flag)
        {
            return Err(Reason::UserInfoOrSubject);
        }
        let (Some(groups), Some(name), Some(username)) = (
            info.strings("groups"),
            info.text("name"),
            info.text("preferred_username"),
        ) else {
            return Err(Reason::UserInfoClaims);
        };
        if !groups
            .iter()
            .any(|group| self.config.oidc_allowed_groups.iter().any(|allowed| allowed == group))
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
            Some(name),
            Some(username),
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
        let (id, secret) = (escape(&self.config.oidc_client_id), escape(&self.secret));
        let credentials = Zeroizing::new(format!("{id}:{secret}"));
        let mut authorization = HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(credentials.as_bytes())))?;
        authorization.set_sensitive(true);
        // Sorted by name, as x/oauth2 encodes them.
        let body = encode(&[
            ("code", code),
            ("code_verifier", verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", &self.redirect_uri()),
        ]);
        let request = Request::post(&provider.token)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::AUTHORIZATION, authorization)
            .body(body)?;
        let response = self.http.call(request).await?;
        if !response.status().is_success() {
            return Err("OIDC token endpoint rejected the exchange".into());
        }
        // As golang.org/x/oauth2 reads it: a form for form and text types, JSON otherwise.
        let form = matches!(
            essence(&response).as_deref(),
            Some("application/x-www-form-urlencoded" | "text/plain")
        );
        let mut fields = serde_json::Map::new();
        if form {
            for (key, value) in form_urlencoded::parse(response.body()) {
                fields.entry(key).or_insert(value.into());
            }
        } else {
            fields = go_json(response.body())?;
        }
        // tokenJSON's strings, where null is empty, and a JSON expires_in in whole seconds.
        let text = |name| fields.get(name).map_or(Some(""), go_str);
        let expiry = fields.get("expires_in").filter(|expiry| !form && !expiry.is_null());
        let access_token = text("access_token").unwrap_or_default();
        if text("error") != Some("")
            || access_token.is_empty()
            || ["token_type", "refresh_token", "error_description", "error_uri"]
                .into_iter()
                .any(|name| text(name).is_none())
            || expiry.is_some_and(|expiry| jwt::go_number(expiry).and_then(|seconds| seconds.as_i64()).is_none())
        {
            return Err("OIDC token endpoint rejected the exchange".into());
        }
        Ok(Tokens {
            access_token: access_token.to_owned(),
            id_token: fields.get("id_token").cloned(),
        })
    }
    async fn user_info(&self, provider: &Provider, access_token: &str) -> Result<Members, ConfigError> {
        let mut bearer = HeaderValue::from_str(&format!("Bearer {access_token}"))?;
        bearer.set_sensitive(true);
        let mut request = get(&provider.userinfo)?;
        request.headers_mut().insert(header::AUTHORIZATION, bearer);
        let response = self.http.call(request).await?;
        if response.status() != StatusCode::OK {
            return Err("OIDC user information unavailable".into());
        }
        if essence(&response).as_deref() != Some("application/jwt") {
            return Ok(go_json(response.body())?);
        }
        let token = std::str::from_utf8(response.body())?;
        let verified = provider
            .verify(&self.http, token)
            .await
            .map_err(|_| "OIDC user information signature rejected")?;
        let claims = go_json(&verified.payload)?;
        jwt::audience_and_issuer(&claims, &self.config.oidc_issuer, &self.config.oidc_client_id)
            .map_err(|_| "OIDC user information claims rejected")?;
        Ok(go_json(&verified.payload)?)
    }
}

struct Tokens {
    access_token: String,
    id_token: Option<Value>,
}

/// An object's members in order, read as Go's decoder fills a struct: a member sets the field whose name it
/// matches case-insensitively, as by Unicode simple folding, so the last of them wins.
struct Members(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for Members {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(Members(Vec::new()))
    }
}

impl<'de> serde::de::Visitor<'de> for Members {
    type Value = Self;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a JSON object")
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(mut self, mut members: A) -> Result<Self, A::Error> {
        while let Some(member) = members.next_entry()? {
            self.0.push(member);
        }
        Ok(self)
    }
}

impl Members {
    fn named(&self, name: &'static str) -> impl Iterator<Item = &Value> {
        let fold = |c: char| match c {
            'ſ' => 's',
            '\u{212a}' => 'k',
            c => c.to_ascii_lowercase(),
        };
        self.0
            .iter()
            .filter(move |(key, _)| key.chars().map(fold).eq(name.chars()))
            .map(|(_, value)| value)
    }
    /// A string field, which a null leaves as it was; None where a member is another type.
    fn text(&self, name: &'static str) -> Option<&str> {
        self.named(name).try_fold(
            "",
            |text, value| if value.is_null() { Some(text) } else { value.as_str() },
        )
    }
    /// A list of strings, which a null empties; a null element is empty.
    fn strings(&self, name: &'static str) -> Option<Vec<&str>> {
        self.named(name).try_fold(Vec::new(), |_, value| match value {
            Value::Null => Some(Vec::new()),
            value => value.as_array()?.iter().map(go_str).collect(),
        })
    }
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
    Ok(Request::get(url).body(String::new())?)
}

/// Go's url.Values.Encode of `pairs`, which are sorted by name.
fn encode(pairs: &[(&str, &str)]) -> String {
    let pair = |(name, value): &(&str, &str)| format!("{name}={}", escape(value));
    pairs.iter().map(pair).collect::<Vec<_>>().join("&")
}

/// Go's url.QueryEscape, which keeps '~' and escapes '*' unlike a form encoder.
fn escape(value: &str) -> String {
    let escaped: String = form_urlencoded::byte_serialize(value.as_bytes()).collect();
    escaped.replace("%7E", "~").replace('*', "%2A")
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
        let mut request = get(url)?;
        let headers = request.headers_mut();
        // As go-oidc asks, so that no cache answers with the keys a rotation replaced.
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        let response = self.call(request).await?;
        Jwks::parse(ok(&response)?).map_err(|_| "OIDC key set is malformed".into())
    }
    async fn call(&self, mut request: Request<String>) -> Result<Response<Vec<u8>>, ConfigError> {
        tokio::time::timeout(PROVIDER_TIMEOUT, async {
            let uri = request.uri().to_string();
            valid_url(&uri)?;
            let (origin, path) = split_url(&uri)?;
            let host = origin.key().split_off("https://".len());
            let agent = format!("graphite-meter/{}", crate::config::ENGINE_VERSION);
            let headers = request.headers_mut();
            headers.insert(header::HOST, HeaderValue::from_str(&host)?);
            // Go's client names itself; a provider's firewall may refuse a request that does not.
            headers.insert(header::USER_AGENT, HeaderValue::from_str(&agent)?);
            *request.uri_mut() = if path.is_empty() { "/".parse()? } else { path.parse()? };
            // Its configuration offers no protocol, so it serves an HTTPS proxy's hop as well.
            let hop = std::future::ready(Ok::<_, std::io::Error>(self.tls.clone()));
            let connection = connect(&self.proxy, &origin, Some(&self.tls), hop).await?;
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
#[path = "oidc_tests.rs"]
pub(super) mod tests;

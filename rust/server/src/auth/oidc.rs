//! Provider interaction and single-use, browser-bound authorization transactions.
use super::{
    SessionLease,
    password_login::read_secret,
    session::{random_token, token_hash},
};
use crate::config::{AuthConfig, ConfigError};
use openidconnect::{core::*, *};
use rustls_platform_verifier::BuilderVerifierExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Mutex as AsyncMutex, Semaphore},
    time::Instant,
};
use zeroize::{Zeroize, Zeroizing};

type Client = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;
type Metadata = ProviderMetadata<
    ExtraMetadata,
    CoreAuthDisplay,
    CoreClientAuthMethod,
    CoreClaimName,
    CoreClaimType,
    CoreGrantType,
    CoreJweContentEncryptionAlgorithm,
    CoreJweKeyManagementAlgorithm,
    CoreJsonWebKey,
    CoreResponseMode,
    CoreResponseType,
    CoreSubjectIdentifierType,
>;
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ExtraMetadata {
    #[serde(default)]
    authorization_response_iss_parameter_supported: bool,
}
impl AdditionalProviderMetadata for ExtraMetadata {}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Groups {
    groups: Vec<String>,
}
impl AdditionalClaims for Groups {}

pub(super) struct Provider {
    client: Client,
    issuer_parameter: bool,
    pub origin: String,
}
struct Transaction {
    provider: Arc<Provider>,
    browser: [u8; 32],
    nonce: Nonce,
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
        let metadata = tokio::time::timeout(
            Duration::from_secs(25),
            Metadata::discover_async(IssuerUrl::new(self.config.oidc_issuer.clone())?, &self.http),
        )
        .await
        .map_err(|_| "OIDC discovery timed out")?
        .map_err(|_| "OIDC provider discovery failed")?;
        for endpoint in [
            Some(metadata.authorization_endpoint().url()),
            metadata.token_endpoint().as_ref().map(|url| url.url()),
            metadata.userinfo_endpoint().as_ref().map(|url| url.url()),
            Some(metadata.jwks_uri().url()),
        ] {
            valid_url(endpoint.ok_or("OIDC endpoint missing")?)?;
        }
        let origin = metadata
            .authorization_endpoint()
            .url()
            .origin()
            .ascii_serialization();
        let issuer_parameter = metadata
            .additional_metadata()
            .authorization_response_iss_parameter_supported;
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(self.config.oidc_client_id.clone()),
            Some(ClientSecret::new(self.secret.to_string())),
        )
        .set_redirect_uri(RedirectUrl::new(format!(
            "{}/auth/oidc/callback",
            self.config.public_url
        ))?);
        let provider = Arc::new(Provider {
            client,
            issuer_parameter,
            origin,
        });
        discovery.provider = Some(provider.clone());
        Ok(provider)
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
        let nonce = random_token::<32>().map_err(|_| "random source unavailable")?;
        let verifier = random_token::<32>().map_err(|_| "random source unavailable")?;
        let pkce =
            PkceCodeChallenge::from_code_verifier_sha256(&PkceCodeVerifier::new(verifier.clone()));
        let (url, state, nonce) = provider
            .client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                || CsrfToken::new(state),
                || Nonce::new(nonce),
            )
            .add_scope(Scope::new("profile".into()))
            .add_scope(Scope::new("groups".into()))
            .set_pkce_challenge(pkce)
            .url();
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
            token_hash(state.secret()),
            Transaction {
                provider,
                browser: token_hash(&browser),
                nonce,
                verifier: Zeroizing::new(verifier),
                deadline: now + Duration::from_secs(600),
                address,
                challenge,
                prior,
            },
        );
        Ok(Started {
            url: url.into(),
            browser,
        })
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
        let client = &tx.provider.client;
        let tokens = client
            .exchange_code(AuthorizationCode::new(code.to_owned()))?
            .set_pkce_verifier(PkceCodeVerifier::new(tx.verifier.to_string()))
            .request_async(&self.http)
            .await
            .map_err(|_| "OIDC token exchange failed")?;
        let token = tokens.id_token().ok_or("OIDC ID token missing")?;
        let verifier = client.id_token_verifier();
        let claims = token
            .claims(&verifier, &tx.nonce)
            .map_err(|_| "OIDC ID token rejected")?;
        if claims
            .authorized_party()
            .is_some_and(|party| party.as_str() != self.config.oidc_client_id)
        {
            return Err("OIDC authorized party rejected".into());
        }
        if let Some(expected) = claims.access_token_hash() {
            let actual = AccessTokenHash::from_token(
                tokens.access_token(),
                token.signing_alg()?,
                token.signing_key(&verifier)?,
            )?;
            if actual != *expected {
                return Err("OIDC access token binding rejected".into());
            }
        }
        let info: UserInfoClaims<Groups, CoreGenderClaim> = client
            .user_info(
                tokens.access_token().clone(),
                Some(claims.subject().clone()),
            )?
            .request_async(&self.http)
            .await
            .map_err(|_| "OIDC user information rejected")?;
        if !info
            .additional_claims()
            .groups
            .iter()
            .any(|group| self.config.oidc_allowed_groups.contains(group))
        {
            return Err("OIDC group denied".into());
        }
        let subject = claims.subject().as_str();
        if subject.is_empty() || subject.len() > 256 || subject.chars().any(char::is_control) {
            return Err("OIDC subject rejected".into());
        }
        let name = [
            info.name()
                .and_then(|name| name.get(None))
                .map(|name| name.as_str()),
            info.preferred_username().map(|name| name.as_str()),
            claims
                .name()
                .and_then(|name| name.get(None))
                .map(|name| name.as_str()),
            claims.preferred_username().map(|name| name.as_str()),
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
}
fn valid_url(url: &url::Url) -> Result<(), ConfigError> {
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "OIDC endpoint must be an absolute HTTPS URL without credentials or fragment".into(),
        );
    }
    Ok(())
}
struct ProviderHttp(reqwest::Client);
impl ProviderHttp {
    fn new() -> Result<Self, ConfigError> {
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth();
        Ok(Self(
            reqwest::Client::builder()
                .use_preconfigured_tls(tls)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15))
                .build()?,
        ))
    }
}
impl<'a> AsyncHttpClient<'a> for ProviderHttp {
    type Error = std::io::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<HttpResponse, Self::Error>> + Send + 'a>,
    >;
    fn call(&'a self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            tokio::time::timeout(
                if request.method() == http::Method::GET {
                    Duration::from_secs(10)
                } else {
                    Duration::from_secs(15)
                },
                async {
                    let url = url::Url::parse(&request.uri().to_string())
                        .map_err(std::io::Error::other)?;
                    valid_url(&url).map_err(|_| std::io::Error::other("invalid OIDC endpoint"))?;
                    let (parts, body) = request.into_parts();
                    let mut response = self
                        .0
                        .request(parts.method, url)
                        .headers(parts.headers)
                        .body(body)
                        .send()
                        .await
                        .map_err(std::io::Error::other)?;
                    let mut output = http::Response::builder().status(response.status());
                    *output.headers_mut().expect("response headers") = response.headers().clone();
                    let mut bytes = Vec::new();
                    while let Some(chunk) = response.chunk().await.map_err(std::io::Error::other)? {
                        if bytes.len() + chunk.len() > 1024 * 1024 {
                            return Err(std::io::Error::other("OIDC response too large"));
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    output.body(bytes).map_err(std::io::Error::other)
                },
            )
            .await
            .map_err(|_| std::io::Error::other("OIDC request timed out"))?
        })
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
        let metadata: CoreProviderMetadata = serde_json::from_value(serde_json::json!({
            "issuer": "https://identity.example",
            "authorization_endpoint": "https://identity.example/authorize",
            "token_endpoint": "https://identity.example/token",
            "userinfo_endpoint": "https://identity.example/userinfo",
            "jwks_uri": "https://identity.example/jwks",
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"]
        }))
        .unwrap();
        let provider = Provider {
            client: CoreClient::from_provider_metadata(
                metadata,
                ClientId::new("meter".into()),
                Some(ClientSecret::new("secret".into())),
            )
            .set_redirect_uri(
                RedirectUrl::new("https://meter.example/auth/oidc/callback".into()).unwrap(),
            ),
            issuer_parameter: true,
            origin: "https://identity.example".into(),
        };
        *oidc.discovery.lock().await = Discovery {
            provider: Some(Arc::new(provider)),
            retry: Instant::now() + Duration::from_secs(30),
        };
        oidc
    }

    #[tokio::test]
    async fn authorization_is_pkce_bound_bounded_and_consumed_before_browser_validation() {
        let oidc = ready().await;
        let address = "192.0.2.1".parse().unwrap();
        let started = oidc.start(address, String::new(), None).await.unwrap();
        let url = url::Url::parse(&started.url).unwrap();
        let fields: HashMap<_, _> = url.query_pairs().into_owned().collect();
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
        let url = url::Url::parse(&started.url).unwrap();
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
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
            assert!(valid_url(&url::Url::parse(endpoint).unwrap()).is_err());
        }
        assert!(valid_url(&url::Url::parse("https://identity.example/token").unwrap()).is_ok());
    }
}

#[cfg(test)]
#[path = "oidc_tests.rs"]
mod provider_tests;

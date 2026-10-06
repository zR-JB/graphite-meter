//! OIDC sign-in as a confidential client: browser-bound state, nonce and PKCE, routes, code exchange, ID token, groups.

use super::{
    Counter, Enabled, LoginKey, Reason, Security, jwt,
    page::{self, Page},
    policy::{cookie, cookie_lease},
    provider::{Client, Provider},
    rate::{Attempts, engage, share_full},
    routes::{check_csrf, clear_cookie, establish, field, form, redirect, rejected, set_cookie},
    store::{Digest, digest, random},
};
use crate::{
    app::{query, response},
    config, lock, log,
    peer::{ClientKey, ClientKeys, Peer},
    transport::body::Body,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use graphite_meter_proto::{approval, origin::Origin, text};
use http::{HeaderValue, Request, Response, header, request::Parts};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime},
};
use subtle::ConstantTimeEq;
use tokio::time::{Instant, sleep, timeout_at};
use zeroize::Zeroizing;

const MAX_TRANSACTIONS: usize = 16384;
const MAX_CLIENT_TRANSACTIONS: usize = 8;
/// A sign-in at the provider must return within this.
const TRANSACTION_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// A callback's provider requests leave this long of its exchange's bound to answer.
const ANSWER_MARGIN: Duration = Duration::from_secs(3);
/// Discovery retries back off from one second, doubling up to this.
const RETRY_MAX: Duration = Duration::from_secs(60);
/// The transaction's cookie, the one a browser sends on the provider's cross-site redirect back.
pub(super) const TRANSACTION_COOKIE: &str = "__Host-gm_oidc";

/// A sign-in sent to the provider, keyed by its state's digest.
struct Transaction {
    provider: Arc<Provider>,
    /// The digest of the browser's transaction cookie.
    browser: Digest,
    nonce: Zeroizing<String>,
    verifier: Zeroizing<String>,
    deadline: Instant,
    clients: Vec<ClientKey>,
    /// The terminal approval to show after sign-in, or empty.
    challenge: String,
    /// The login the browser presented at the start, which the new one replaces.
    prior: Option<LoginKey>,
}

pub(super) struct Oidc {
    settings: config::Oidc,
    redirect: String,
    secret: Zeroizing<String>,
    client: Client,
    provider: OnceLock<Arc<Provider>>,
    /// The transactions, and when their ceiling was last logged.
    transactions: Mutex<(HashMap<Digest, Transaction>, Option<Instant>)>,
    starts: Attempts,
    exchanges: Attempts,
}

impl Oidc {
    pub fn new(public: &Origin, settings: &config::Oidc, secret: Zeroizing<String>) -> Result<Self, String> {
        Ok(Self {
            settings: settings.clone(),
            redirect: format!("{public}/auth/oidc/callback"),
            secret,
            client: Client::new()?,
            provider: OnceLock::new(),
            transactions: Mutex::default(),
            starts: Attempts::new("oidc-start", 10),
            exchanges: Attempts::new("oidc-exchange", 10),
        })
    }

    /// The provider once discovered.
    pub fn provider(&self) -> Option<&Arc<Provider>> {
        self.provider.get()
    }

    /// Discovers the provider unless it is known.
    pub async fn discover(&self, security: &Security) -> Result<(), String> {
        if self.provider.get().is_some() {
            return Ok(());
        }
        let provider = Provider::discover(&self.client, &self.settings.issuer).await;
        let provider = provider.inspect_err(|error| security.debug(format_args!("OIDC discovery failed: {error}")))?;
        if self.provider.set(Arc::new(provider)).is_ok() {
            log!(Info, "auth", "OIDC provider ready");
        }
        Ok(())
    }

    /// Discovers the provider, retrying with backoff until it answers; then waits forever.
    pub async fn retry(&self, security: &Security) {
        let mut delay = Duration::from_secs(1);
        for attempt in 0.. {
            if self.discover(security).await.is_ok() {
                break;
            }
            match attempt {
                0 => log!(Warn, "auth", "OIDC provider unavailable; password sign-in stays available; retrying"),
                _ => log!(Warn, "auth", "OIDC provider unavailable; retrying"),
            }
            sleep(delay).await;
            delay = (delay * 2).min(RETRY_MAX);
        }
        std::future::pending().await
    }

    /// Opens a transaction for `keys`, returning the provider's sign-in URL and the transaction cookie's value.
    fn start(&self, keys: &ClientKeys, challenge: &str, prior: Option<LoginKey>) -> Result<(String, String), Reason> {
        let provider = self.provider.get().ok_or(Reason::ProviderNotReady)?.clone();
        let (browser, state) = (random::<32>(), random::<32>());
        let (nonce, verifier) = (Zeroizing::new(random::<32>()), Zeroizing::new(random::<32>()));
        let url = provider.sign_in(&query::encode(&[
            ("client_id", &self.settings.client_id),
            ("code_challenge", &approval::challenge(&verifier)),
            ("code_challenge_method", "S256"),
            ("nonce", &nonce),
            ("redirect_uri", &self.redirect),
            ("response_type", "code"),
            ("scope", "openid profile groups"),
            ("state", &state),
        ]));
        let now = Instant::now();
        let mut transactions = lock(&self.transactions);
        let (table, logged) = &mut *transactions;
        table.retain(|_, transaction| now < transaction.deadline);
        let full = table.len() >= MAX_TRANSACTIONS;
        if full {
            engage(logged, now, format_args!("oidc-transaction"));
        }
        let held = |key: &ClientKey| table.values().filter(|held| held.clients.contains(key)).count();
        if full || share_full(keys, MAX_CLIENT_TRANSACTIONS, held) {
            return Err(Reason::TransactionCapacity);
        }
        let transaction = Transaction {
            provider,
            browser: digest(&browser),
            nonce,
            verifier,
            deadline: now + TRANSACTION_LIFETIME,
            clients: keys.iter().collect(),
            challenge: challenge.into(),
            prior,
        };
        table.insert(digest(&state), transaction);
        Ok((url, browser))
    }

    /// Ends `state`'s transaction for cookie `browser`, checking `issuer`; a refusal carries its challenge if found.
    fn take(&self, state: &str, browser: &str, issuer: Option<&str>) -> Result<Transaction, (Reason, String)> {
        let found = lock(&self.transactions).0.remove(&digest(state));
        let Some(transaction) = found else {
            return Err((Reason::TransactionReplay, String::new()));
        };
        let bound = bool::from(transaction.browser.ct_eq(&digest(browser)));
        if Instant::now() >= transaction.deadline || !bound {
            return Err((Reason::TransactionReplay, transaction.challenge));
        }
        let (issuer, expected) = (issuer.unwrap_or_default(), &self.settings.issuer);
        if issuer != expected && (transaction.provider.issuer_parameter || !issuer.is_empty()) {
            return Err((Reason::ResponseIssuer, transaction.challenge));
        }
        Ok(transaction)
    }

    /// The login subject and display name the provider vouches for.
    async fn identify(&self, transaction: &Transaction, code: &str) -> Result<(String, String), Reason> {
        #[derive(Deserialize)]
        struct Info {
            sub: Option<String>,
            name: Option<String>,
            preferred_username: Option<String>,
            groups: Option<Vec<String>>,
        }
        let (provider, settings) = (&transaction.provider, &self.settings);
        let (access_token, id_token) = self.exchange(provider, code, &transaction.verifier).await?;
        let verified = provider.verify(&self.client, &id_token).await;
        let verified = verified.ok_or(Reason::IdTokenVerification)?;
        let (issuer, client) = (&settings.issuer, &settings.client_id);
        let (subject, names) = jwt::id_token(&verified, issuer, client, &transaction.nonce, &access_token)?;
        let info = self.user_info(provider, &access_token).await;
        let info = info.ok_or(Reason::UserInfoOrSubject)?;
        let info: Info = serde_json::from_slice(&info).map_err(|_| Reason::UserInfoClaims)?;
        if info.sub.as_ref() != Some(&subject) {
            return Err(Reason::UserInfoOrSubject);
        }
        let groups = info.groups.unwrap_or_default();
        if !groups.iter().any(|group| settings.allowed_groups.contains(group)) {
            return Err(Reason::GroupDenied);
        }
        if subject.is_empty() || subject.len() > 256 || !subject.chars().all(text::safe) {
            return Err(Reason::InvalidSubject);
        }
        let mut offered = [info.name, info.preferred_username].into_iter().chain(names).flatten();
        let name = offered.find(|name| !name.is_empty()).unwrap_or_else(|| subject.clone());
        let name = text::clean(&name, 64);
        let name = match name.trim() {
            "" => "OIDC user",
            name => name,
        };
        Ok((format!("oidc:{subject}"), name.to_owned()))
    }

    /// The access token and ID token `code` exchanges for, authenticated with the client secret.
    async fn exchange(&self, provider: &Provider, code: &str, verifier: &str) -> Result<(String, String), Reason> {
        #[derive(Deserialize)]
        struct Tokens {
            #[serde(default)]
            access_token: String,
            id_token: Option<Value>,
        }
        let id = query::escape(&self.settings.client_id);
        let credentials = Zeroizing::new(format!("{id}:{}", query::escape(&self.secret)));
        let basic = Zeroizing::new(format!("Basic {}", STANDARD.encode(credentials.as_bytes())));
        let mut authorization = HeaderValue::from_str(&basic).map_err(|_| Reason::TokenExchange)?;
        authorization.set_sensitive(true);
        let form = query::encode(&[
            ("code", code),
            ("code_verifier", verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", &self.redirect),
        ]);
        let authorization = Some((header::AUTHORIZATION, authorization));
        let answer = self.client.send(&provider.token, authorization, Some(form)).await;
        let tokens = answer
            .ok()
            .and_then(|answer| serde_json::from_slice::<Tokens>(&answer.body).ok());
        let tokens = tokens.filter(|tokens| !tokens.access_token.is_empty());
        let tokens = tokens.ok_or(Reason::TokenExchange)?;
        let Some(Value::String(id_token)) = tokens.id_token else {
            return Err(Reason::MissingIdToken);
        };
        Ok((tokens.access_token, id_token))
    }

    /// The user information document; a signed one must verify and name the issuer and this client.
    async fn user_info(&self, provider: &Provider, access_token: &str) -> Option<Vec<u8>> {
        let mut bearer = HeaderValue::from_str(&format!("Bearer {access_token}")).ok()?;
        bearer.set_sensitive(true);
        let bearer = Some((header::AUTHORIZATION, bearer));
        let answer = self.client.send(&provider.userinfo, bearer, None).await.ok()?;
        if answer.media != "application/jwt" {
            return Some(answer.body);
        }
        let token = std::str::from_utf8(&answer.body).ok()?;
        let verified = provider.verify(&self.client, token).await?;
        let (issuer, client) = (&self.settings.issuer, &self.settings.client_id);
        jwt::addressed(&verified.payload, issuer, client).then_some(verified.payload)
    }
}

/// `POST /auth/oidc/start`: the form's CSRF proof and the start budget, then a transaction and the way to the provider.
pub(super) async fn start<B: http_body::Body>(
    auth: &Enabled,
    oidc: &Oidc,
    request: Request<B>,
    peer: &Peer,
) -> Response<Body> {
    let (head, form) = form(request).await;
    let challenge = form.as_deref().map_or("", |form| field(form, "challenge"));
    let started = (|| {
        oidc.provider().ok_or(Reason::ProviderNotReady)?;
        check_csrf(auth, &head.headers, field(form.as_deref().ok_or(Reason::MalformedForm)?, "csrf"))?;
        let keys = peer.keys().filter(|keys| oidc.starts.allow(keys, false, None));
        let keys = keys.ok_or(Reason::Throttled)?;
        let approval = if approval::verification_code(challenge).is_some() { challenge } else { "" };
        oidc.start(&keys, approval, cookie_lease(&auth.store, &head.headers).map(|lease| lease.login()))
    })();
    match started {
        Ok((url, browser)) => {
            let mut answer = redirect(&url);
            set_cookie(&mut answer, TRANSACTION_COOKIE, &browser, SystemTime::now() + TRANSACTION_LIFETIME);
            answer
        }
        Err(reason) => refused(auth, reason, challenge),
    }
}

/// `GET /auth/oidc/callback`: the named transaction, exchange budget, then the provider's word by `deadline`.
pub(super) async fn callback(
    auth: &Enabled,
    oidc: &Oidc,
    head: Parts,
    deadline: Instant,
    peer: &Peer,
) -> Response<Body> {
    let mut answer = match sign_in(auth, oidc, &head, deadline, peer).await {
        Ok(answer) => answer,
        Err((reason, challenge)) if challenge.is_empty() => {
            refused(auth, reason, &query::get(head.uri.query(), "challenge").unwrap_or_default())
        }
        Err((reason, challenge)) => refused(auth, reason, &challenge),
    };
    clear_cookie(&mut answer, TRANSACTION_COOKIE);
    answer
}

async fn sign_in(
    auth: &Enabled,
    oidc: &Oidc,
    head: &Parts,
    deadline: Instant,
    peer: &Peer,
) -> Result<Response<Body>, (Reason, String)> {
    let (query, refuse) = (head.uri.query(), |reason| (reason, String::new()));
    let values = |name| query::values(query, name).collect::<Vec<_>>();
    let (code, state, issuer) = (values("code"), values("state"), values("iss"));
    let ([code], [state]) = (&code[..], &state[..]) else {
        return Err(refuse(Reason::CallbackParameters));
    };
    let printable = code.bytes().all(|byte| (0x20..0x7f).contains(&byte));
    if code.is_empty() || state.is_empty() || !printable || issuer.len() > 1 || query::get(query, "error").is_some() {
        return Err(refuse(Reason::CallbackParameters));
    }
    let browser = cookie(&head.headers, TRANSACTION_COOKIE).ok_or(refuse(Reason::TransactionCookie))?;
    let transaction = oidc.take(state, browser, issuer.first().map(String::as_str))?;
    let fail = |reason| (reason, transaction.challenge.clone());
    if !peer.keys().is_some_and(|keys| oidc.exchanges.allow(&keys, false, None)) {
        return Err(fail(Reason::ExchangeRateLimited));
    }
    let identify = timeout_at(deadline - ANSWER_MARGIN, oidc.identify(&transaction, code));
    let (subject, name) = identify.await.unwrap_or(Err(Reason::TokenExchange)).map_err(fail)?;
    let page = response::html(page::render(Page::Continue, &[("Challenge", &transaction.challenge)]));
    let answer = establish(auth, (&subject, &name, &auth.provider), transaction.prior, page).map_err(fail)?;
    auth.security.count(Counter::Oidc);
    log!(Info, "auth", "signed in: {name}, through {}", auth.provider);
    Ok(answer)
}

/// An OIDC refusal, which the security log also counts as an OIDC failure.
fn refused(auth: &Enabled, reason: Reason, challenge: &str) -> Response<Body> {
    auth.security.count(Counter::OidcFailure);
    rejected(auth, reason, challenge)
}

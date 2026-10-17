//! Signs in by OIDC as a confidential client with browser-bound state, nonce, PKCE, code exchange, ID token and groups.

use super::{
    Counter, Enabled, Identity, LoginKey, Principal, Reason, Security,
    approval::Challenge,
    jwt, page,
    policy::{Cookie, cookie, cookie_lease},
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
const TRANSACTION_LIFETIME: Duration = Duration::from_mins(10);
/// A callback's provider requests leave this long of its exchange's bound to answer.
const ANSWER_MARGIN: Duration = Duration::from_secs(3);
/// Discovery retries back off from one second, doubling up to this.
const RETRY_MAX: Duration = Duration::from_mins(1);

/// Records a sign-in sent to the provider, keyed by its state's digest.
struct Transaction {
    provider: Arc<Provider>,
    /// Digests the browser's transaction cookie.
    browser: Digest,
    nonce: Zeroizing<String>,
    verifier: Zeroizing<String>,
    deadline: Instant,
    clients: Vec<ClientKey>,
    /// The page shows this approval after sign-in.
    challenge: Option<Challenge>,
    /// The browser presented this login at the start, and the new one replaces it.
    prior: Option<LoginKey>,
}

pub(super) struct Oidc {
    settings: config::Oidc,
    redirect: String,
    secret: Zeroizing<String>,
    client: Client,
    provider: OnceLock<Arc<Provider>>,
    /// Holds the transactions, and when their ceiling was last logged.
    transactions: Mutex<(HashMap<Digest, Transaction>, Option<Instant>)>,
    starts: Attempts,
    exchanges: Attempts,
}

impl Oidc {
    pub fn new(
        public: &Origin,
        settings: &config::Oidc,
        secret: Zeroizing<String>,
        outbound: &config::Outbound,
    ) -> Result<Self, String> {
        Ok(Self {
            settings: settings.clone(),
            redirect: format!("{public}/auth/oidc/callback"),
            secret,
            client: Client::new(outbound)?,
            provider: OnceLock::new(),
            transactions: Mutex::default(),
            starts: Attempts::new("oidc-start", 10),
            exchanges: Attempts::new("oidc-exchange", 10),
        })
    }

    /// Gives the provider once discovered.
    pub fn provider(&self) -> Option<&Arc<Provider>> {
        self.provider.get()
    }

    /// Discovers the provider unless it is known.
    pub async fn discover(&self) -> Result<(), String> {
        if self.provider.get().is_some() {
            return Ok(());
        }
        let provider = Provider::discover(&self.client, &self.settings.issuer).await?;
        if self.provider.set(Arc::new(provider)).is_ok() {
            log!(Info, "auth", "OIDC provider ready");
        }
        Ok(())
    }

    /// Discovers the provider, retrying with backoff until it answers.
    pub async fn retry(&self) {
        let mut delay = Duration::from_secs(1);
        while let Err(error) = self.discover().await {
            let retry = log::span(delay);
            log!(
                Warn,
                "auth",
                "OIDC provider unavailable: {error}; retrying in {retry}, password sign-in still works"
            );
            sleep(delay).await;
            delay = (delay * 2).min(RETRY_MAX);
        }
    }

    /// Opens a transaction for `keys`, returning the provider's sign-in URL and the transaction cookie's value.
    fn start(
        &self,
        keys: &ClientKeys,
        challenge: Option<Challenge>,
        prior: Option<LoginKey>,
    ) -> Result<(String, String), Reason> {
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
            challenge,
            prior,
        };
        table.insert(digest(&state), transaction);
        Ok((url, browser))
    }

    /// Ends `state`'s transaction for cookie `browser`, checking `issuer`. A refusal carries its challenge if found.
    fn take(
        &self,
        state: &str,
        browser: &str,
        issuer: Option<&str>,
    ) -> Result<Transaction, (Reason, Option<Challenge>)> {
        let found = lock(&self.transactions).0.remove(&digest(state));
        let Some(transaction) = found else {
            return Err((Reason::TransactionReplay, None));
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

    /// Gets the login principal and display name the provider vouches for. Provider failures go to `security`'s log.
    async fn identify(
        &self,
        transaction: &Transaction,
        code: &str,
        security: &Security,
    ) -> Result<(Principal, String), Reason> {
        #[derive(Deserialize)]
        struct Info {
            sub: Option<String>,
            name: Option<String>,
            preferred_username: Option<String>,
            groups: Option<Vec<String>>,
        }
        let (provider, settings) = (&transaction.provider, &self.settings);
        let (access_token, id_token) = self.exchange(provider, code, &transaction.verifier, security).await?;
        let verified = provider.verify(&self.client, &id_token).await;
        let verified = verified.ok_or(Reason::IdTokenVerification)?;
        let (issuer, client) = (&settings.issuer, &settings.client_id);
        let (subject, names) = jwt::id_token(&verified, issuer, client, &transaction.nonce, &access_token)?;
        let info = self.user_info(provider, &access_token, security).await;
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
        Ok((Principal::oidc(&subject), name.to_owned()))
    }

    /// Exchanges `code` for the access token and ID token, authenticated with the client secret.
    async fn exchange(
        &self,
        provider: &Provider,
        code: &str,
        verifier: &str,
        security: &Security,
    ) -> Result<(String, String), Reason> {
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
        let answer = answer.inspect_err(|error| security.debug(format_args!("OIDC token exchange failed: {error}")));
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

    /// Fetches the user information document. A signed one must verify and name the issuer and this client.
    async fn user_info(&self, provider: &Provider, access_token: &str, security: &Security) -> Option<Vec<u8>> {
        let mut bearer = HeaderValue::from_str(&format!("Bearer {access_token}")).ok()?;
        bearer.set_sensitive(true);
        let bearer = Some((header::AUTHORIZATION, bearer));
        let answer = self.client.send(&provider.userinfo, bearer, None).await;
        let answer = answer.inspect_err(|error| security.debug(format_args!("OIDC user information failed: {error}")));
        let answer = answer.ok()?;
        if answer.media != "application/jwt" {
            return Some(answer.body);
        }
        let token = std::str::from_utf8(&answer.body).ok()?;
        let verified = provider.verify(&self.client, token).await?;
        let (issuer, client) = (&self.settings.issuer, &self.settings.client_id);
        jwt::addressed(&verified.payload, issuer, client).then_some(verified.payload)
    }
}

/// `POST /auth/oidc/start` checks the CSRF proof and start budget, then opens a transaction and points to the provider.
pub(super) async fn start<B: http_body::Body>(
    auth: &Enabled,
    oidc: &Oidc,
    request: Request<B>,
    peer: &Peer,
) -> Response<Body> {
    let (head, form) = form(request).await;
    let challenge = form
        .as_deref()
        .and_then(|form| Challenge::parse(field(form, "challenge")));
    let started = (|| {
        oidc.provider().ok_or(Reason::ProviderNotReady)?;
        check_csrf(auth, &head.headers, field(form.as_deref().ok_or(Reason::MalformedForm)?, "csrf"))?;
        let keys = peer.keys().ok_or(Reason::ClientAddress)?;
        let keys = Some(keys)
            .filter(|keys| oidc.starts.allow(keys, false, None))
            .ok_or(Reason::Throttled)?;
        oidc.start(
            &keys,
            challenge.clone(),
            cookie_lease(&auth.store, &head.headers).map(|lease| lease.login()),
        )
    })();
    match started {
        Ok((url, browser)) => {
            let mut answer = redirect(&url);
            set_cookie(&mut answer, Cookie::Transaction, &browser, SystemTime::now() + TRANSACTION_LIFETIME);
            answer
        }
        Err(reason) => refused(auth, reason, challenge.as_ref()),
    }
}

/// `GET /auth/oidc/callback` takes the named transaction and exchange budget, then the provider's word by `deadline`.
pub(super) async fn callback(
    auth: &Enabled,
    oidc: &Oidc,
    head: Parts,
    deadline: Instant,
    peer: &Peer,
) -> Response<Body> {
    let mut answer = match sign_in(auth, oidc, &head, deadline, peer).await {
        Ok(answer) => answer,
        Err((reason, None)) => {
            let challenge = query::get(head.uri.query(), "challenge").and_then(|text| Challenge::parse(&text));
            refused(auth, reason, challenge.as_ref())
        }
        Err((reason, challenge)) => refused(auth, reason, challenge.as_ref()),
    };
    clear_cookie(&mut answer, Cookie::Transaction);
    answer
}

async fn sign_in(
    auth: &Enabled,
    oidc: &Oidc,
    head: &Parts,
    deadline: Instant,
    peer: &Peer,
) -> Result<Response<Body>, (Reason, Option<Challenge>)> {
    let (query, refuse) = (head.uri.query(), |reason| (reason, None));
    let values = |name| query::values(query, name).collect::<Vec<_>>();
    let (code, state, issuer) = (values("code"), values("state"), values("iss"));
    let ([code], [state]) = (&code[..], &state[..]) else {
        return Err(refuse(Reason::CallbackParameters));
    };
    let printable = code.bytes().all(|byte| (0x20..0x7f).contains(&byte));
    if code.is_empty() || state.is_empty() || !printable || issuer.len() > 1 || query::get(query, "error").is_some() {
        return Err(refuse(Reason::CallbackParameters));
    }
    let browser = cookie(&head.headers, Cookie::Transaction).ok_or(refuse(Reason::TransactionCookie))?;
    let transaction = oidc.take(state, browser, issuer.first().map(String::as_str))?;
    let fail = |reason| (reason, transaction.challenge.clone());
    if !peer.keys().is_some_and(|keys| oidc.exchanges.allow(&keys, false, None)) {
        return Err(fail(Reason::ExchangeRateLimited));
    }
    let identify = timeout_at(deadline - ANSWER_MARGIN, oidc.identify(&transaction, code, &auth.security));
    let (principal, name) = identify.await.unwrap_or(Err(Reason::TokenExchange)).map_err(fail)?;
    let challenge = transaction.challenge.as_ref().map(Challenge::as_str);
    let page = response::html(page::Continue { challenge, opening: false }.render());
    let identity = Identity { principal, name: &name, provider: &auth.provider };
    let answer = establish(auth, identity, transaction.prior, page).map_err(fail)?;
    auth.security.count(Counter::Oidc);
    log!(Info, "auth", "signed in: {name}, through {}", auth.provider);
    Ok(answer)
}

/// Refuses an OIDC sign-in, which the security log also counts as an OIDC failure.
fn refused(auth: &Enabled, reason: Reason, challenge: Option<&Challenge>) -> Response<Body> {
    auth.security.count(Counter::OidcFailure);
    rejected(auth, reason, challenge)
}

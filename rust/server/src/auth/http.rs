//! HTTP authentication controller. Requests arrive only after policy authorization.
use super::{
    ApprovalError, ApprovalKind, AuthLease, AuthRoute, Exchange, ExchangeError, SESSION_LIFETIME, SessionLease,
    SessionStore, TicketError,
    logging::{Counter, SecurityLog},
    oidc::{Oidc, media_type},
    pages::{self, LoginPage},
    password_login::{PasswordAttempt, PasswordLogin, check_csrf},
    policy::{Authorization, AuthorizedRequest, Policy, constant_equal, cookie, text},
    rate::{AttemptLimiter, Budget},
    reason::Reason,
    session::random_token,
    ticket::unescape,
    valid_challenge,
};
use crate::{
    config::{AuthConfig, AuthMode, ConfigError},
    cors::Access,
    http::response::{json_response, query_pairs, redirect as go_redirect, text_response},
    log::rfc3339,
    sync::lock,
};
use bytes::Bytes;
use graphite_meter_core::route::{self, Kind, Route};
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use ipnet::IpNet;
use serde::Deserialize;
use serde_json::json;
use std::{
    net::IpAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const FORM_BYTES: usize = 4096;
/// Security counters are logged, as Go's, once a minute when they changed.
const SECURITY_LOG_INTERVAL: Duration = Duration::from_secs(60);
/// The login form's nonce cookie lives ten minutes, as Go's.
const LOGIN_NONCE_LIFETIME: Duration = Duration::from_secs(10 * 60);

pub struct Service {
    policy: Policy,
    sessions: SessionStore,
    password: Option<PasswordLogin>,
    oidc: Option<Oidc>,
    /// Go's OIDCProviderName, which the sign-in page names in every mode.
    provider: String,
    mode: AuthMode,
    log: Arc<SecurityLog>,
    attempts: Arc<AttemptLimiter>,
}

impl Service {
    pub fn new(config: &AuthConfig, trusted: Vec<IpNet>) -> Result<Self, ConfigError> {
        let sessions = SessionStore::default();
        let log = Arc::new(SecurityLog::default());
        let attempts = Arc::new(AttemptLimiter::with_log(log.clone()));
        let service = Self {
            policy: Policy::new(&config.public_url, config.mode, trusted, sessions.clone())?,
            password: config
                .mode
                .password()
                .then(|| PasswordLogin::new(config, sessions.clone(), attempts.clone()))
                .transpose()?,
            oidc: config.mode.oidc().then(|| Oidc::new(config, log.clone())).transpose()?,
            provider: config.oidc_provider_name.clone(),
            mode: config.mode,
            sessions,
            attempts,
            log,
        };
        crate::log!(
            "[gm:auth] mode={} origin={} provider={} issuer={} allowed-groups={} session-lifetime={}",
            config.mode.name(),
            config.public_url,
            config.oidc_provider_name,
            config.oidc_issuer,
            config.oidc_allowed_groups.len(),
            crate::config::go_duration(SESSION_LIFETIME),
        );
        Ok(service)
    }
    pub(crate) fn configure_logging(&self, verbose: bool) {
        self.log.configure(verbose);
        if self.password.is_some() {
            self.log.debug(format_args!("local password hash loaded and validated"));
        }
    }
    pub(crate) async fn security_log(&self) {
        let aggregate = async {
            let mut last = [0; Counter::COUNT];
            let mut ticker = tokio::time::interval(SECURITY_LOG_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if let Some(line) = self.log.window(&mut last) {
                    crate::log!("{line}");
                }
            }
        };
        let provider = async {
            if let Some(oidc) = &self.oidc {
                oidc.retry_discovery().await;
            }
        };
        tokio::join!(aggregate, provider);
    }
    pub async fn initialize(&self) -> Result<(), ConfigError> {
        if self.mode == AuthMode::Oidc
            && let Some(oidc) = &self.oidc
        {
            oidc.discover()
                .await
                .map_err(|error| format!("OIDC discovery: {error}"))?;
        }
        Ok(())
    }
    pub(crate) fn debug(&self, message: std::fmt::Arguments<'_>) {
        self.log.debug(message);
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Answers the controller's own paths and the socket ticket routes, whose bounded bodies the caller collected;
    /// measurement streaming stays with the dispatcher and its lease. A controller path no route claims is not found.
    pub async fn handle(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let request = authorized.request();
        if let Authorization::Authenticated(lease) = authorized.authorization()
            && !lease.is_active()
        {
            return response(StatusCode::FORBIDDEN);
        }
        let path = request.uri().path();
        // As Go's GET patterns, a page answers HEAD too; the policy has already refused one with no session.
        let method = if request.method() == Method::HEAD {
            &Method::GET
        } else {
            request.method()
        };
        match AuthRoute::lookup(method, path) {
            Some(AuthRoute::Login) => self.login_page(request).await,
            Some(AuthRoute::OidcStart) => self.oidc_start(authorized).await,
            Some(AuthRoute::OidcCallback) => self.oidc_callback(authorized).await,
            Some(AuthRoute::Password) => self.password_login(authorized).await,
            Some(AuthRoute::Session) => self.session_info(authorized),
            Some(AuthRoute::Logout) => self.logout(authorized),
            Some(AuthRoute::CliPage) => self.approval_page(authorized, false),
            Some(AuthRoute::BrowserPage) => self.approval_page(authorized, true),
            Some(AuthRoute::CliApprove) => self.approve(authorized, ApprovalKind::Cli),
            Some(AuthRoute::BrowserApprove) => self.approve(authorized, ApprovalKind::Browser),
            Some(AuthRoute::CliToken) => self.exchange(request, false),
            Some(AuthRoute::BrowserToken) => self.exchange(request, true),
            None => match (route::lookup(path), request.method() == Method::POST) {
                (Some(Route::WtSession), true) => self.ticket(authorized, Kind::WebTransport),
                (Some(Route::WsSession), true) => self.ticket(authorized, Kind::WebSocket),
                _ => error_response(StatusCode::NOT_FOUND),
            },
        }
    }

    async fn login_page(&self, request: &Request<Bytes>) -> Response<Bytes> {
        let provider = self.oidc.as_ref().and_then(|oidc| oidc.ready());
        let Ok(nonce) = random_token::<32>() else {
            return response(StatusCode::SERVICE_UNAVAILABLE);
        };
        let query = query_pairs(request);
        let challenge = value(&query, "challenge");
        let challenge = valid_challenge(challenge).map_or("", |_| challenge);
        let notice = match value(&query, "error") {
            "" => "",
            value @ ("provider" | "busy" | "stale" | "throttled" | "password") => value,
            _ => "failed",
        };
        let status = match value(&query, "reason") {
            value @ ("expired" | "renew" | "signed_out") => value,
            _ => "",
        };
        let page = LoginPage {
            csrf: &nonce,
            provider: &self.provider,
            challenge,
            password: self.password.is_some(),
            oidc: self.oidc.is_some(),
            oidc_ready: provider.is_some(),
            notice,
            status,
        };
        let mut result = html(StatusCode::OK, page.render());
        if let Some(provider) = provider {
            result.headers_mut().extend(provider.page_headers.clone());
        }
        let expires = SystemTime::now() + LOGIN_NONCE_LIFETIME;
        set_cookie(&mut result, "__Host-gm_login", &nonce, expires, "Strict");
        result
    }

    async fn password_login(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let request = authorized.request();
        let Some(password) = &self.password else {
            return error_response(StatusCode::NOT_FOUND);
        };
        let Ok(form) = form(request) else {
            return self.rejected(request.method(), Reason::MalformedForm, "");
        };
        let challenge = value(&form, "challenge");
        let result = password
            .attempt(PasswordAttempt {
                client: self.client(authorized),
                origin: text(request.headers(), "origin").unwrap_or_default(),
                nonce_cookie: cookie(request.headers(), "__Host-gm_login"),
                device_cookie: cookie(request.headers(), "__Host-gm_device"),
                csrf: value(&form, "csrf"),
                password: value(&form, "password"),
                prior_session: cookie(request.headers(), "__Host-gm_session"),
            })
            .await;
        match result {
            Ok((token, session)) => {
                self.log.count(Counter::Local);
                let destination = if valid_challenge(challenge).is_some() {
                    query_url(AuthRoute::CliPage.path(), &[("challenge", challenge)])
                } else {
                    "/".into()
                };
                let mut result = redirect(request.method(), &destination);
                session_cookies(&mut result, &token, &session);
                let (device, expires) = password.device_cookie(SystemTime::now());
                set_cookie(&mut result, "__Host-gm_device", &device, expires, "Strict");
                result
            }
            Err(reason) => self.rejected(request.method(), reason, challenge),
        }
    }

    async fn oidc_start(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let Some(oidc) = &self.oidc else {
            return error_response(StatusCode::NOT_FOUND);
        };
        let request = authorized.request();
        let form = form(request);
        let challenge = form.as_ref().map_or("", |form| value(form, "challenge"));
        if oidc.ready().is_none() {
            return self.oidc_rejected(request.method(), Reason::ProviderNotReady, challenge);
        }
        let Ok(form) = &form else {
            return self.oidc_rejected(request.method(), Reason::MalformedForm, "");
        };
        let origin = text(request.headers(), "origin").unwrap_or_default();
        let nonce = cookie(request.headers(), "__Host-gm_login");
        if let Err(reason) = check_csrf(self.policy.public_origin(), origin, nonce, value(form, "csrf")) {
            return self.oidc_rejected(request.method(), reason, challenge);
        }
        let address = self.client(authorized);
        let Some(address) = address.filter(|&address| self.attempts.allow(Budget::OidcStart, address)) else {
            return self.oidc_rejected(request.method(), Reason::Throttled, challenge);
        };
        let prior = cookie(request.headers(), "__Host-gm_session").and_then(|token| self.sessions.lookup(token));
        let stored = valid_challenge(challenge).map_or("", |_| challenge);
        match oidc.start(address, stored.to_owned(), prior).await {
            Ok(started) => {
                let mut result = redirect(request.method(), &started.url);
                result.headers_mut().extend(started.provider.page_headers.clone());
                let expires = SystemTime::now() + super::oidc::TRANSACTION_LIFETIME;
                set_cookie(&mut result, "__Host-gm_oidc", &started.browser, expires, "Lax");
                result
            }
            Err(reason) => self.oidc_rejected(request.method(), reason, challenge),
        }
    }

    async fn oidc_callback(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let Some(oidc) = &self.oidc else {
            return error_response(StatusCode::NOT_FOUND);
        };
        let request = authorized.request();
        let mut result = self
            .oidc_login(oidc, authorized)
            .await
            .unwrap_or_else(|(reason, mut challenge)| {
                // As Go's refusal, which reads the callback's own challenge where the transaction names none.
                if challenge.is_empty() {
                    challenge = value(&query_pairs(request), "challenge").to_owned();
                }
                self.oidc_rejected(request.method(), reason, &challenge)
            });
        clear_cookie(&mut result, "__Host-gm_oidc", "Lax");
        result
    }

    async fn oidc_login(
        &self,
        oidc: &Oidc,
        authorized: &AuthorizedRequest<Bytes>,
    ) -> Result<Response<Bytes>, (Reason, String)> {
        let request = authorized.request();
        let fields = query_pairs(request);
        let unique = |key: &str| {
            let mut values = fields.iter().filter(|(name, _)| name == key);
            let value = values.next().map(|(_, value)| value.as_str());
            if values.next().is_some() { None } else { value }
        };
        let refuse = |reason| (reason, String::new());
        let state = unique("state").filter(|state| !state.is_empty());
        let code =
            unique("code").filter(|code| !code.is_empty() && code.bytes().all(|byte| (0x20..0x7f).contains(&byte)));
        let (Some(state), Some(code)) = (state, code) else {
            return Err(refuse(Reason::CallbackParameters));
        };
        if fields.iter().any(|(key, _)| key == "error") || fields.iter().filter(|(key, _)| key == "iss").count() > 1 {
            return Err(refuse(Reason::CallbackParameters));
        }
        let browser = cookie(request.headers(), "__Host-gm_oidc").ok_or(refuse(Reason::TransactionCookie))?;
        let tx = oidc.take(state, browser, unique("iss"))?;
        let fail = |reason| (reason, tx.challenge.clone());
        let address = self.client(authorized);
        if !address.is_some_and(|address| self.attempts.allow(Budget::OidcExchange, address)) {
            return Err(fail(Reason::ExchangeRateLimited));
        }
        let identity = oidc.complete(&tx, code).await.map_err(fail)?;
        let (token, session) = self
            .sessions
            .create(&identity.subject, &identity.name, &self.provider, None)
            .map_err(|_| fail(Reason::SessionCapacity))?;
        if let Some(prior) = &tx.prior {
            self.sessions.revoke(prior);
        }
        let mut result = html(StatusCode::OK, pages::continue_page(&tx.challenge, false));
        session_cookies(&mut result, &token, &session);
        self.log.count(Counter::Oidc);
        Ok(result)
    }

    fn oidc_rejected(&self, method: &Method, reason: Reason, challenge: &str) -> Response<Bytes> {
        self.log.count(Counter::OidcFailure);
        self.rejected(method, reason, challenge)
    }

    fn rejected(&self, method: &Method, reason: Reason, challenge: &str) -> Response<Bytes> {
        self.log.refused(reason);
        let challenge = valid_challenge(challenge).map(|_| ("challenge", challenge));
        let fields: Vec<_> = challenge.into_iter().chain([("error", reason.notice())]).collect();
        redirect(method, &query_url(AuthRoute::Login.path(), &fields))
    }

    fn session_info(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let Some(lease) = principal(authorized) else {
            return response(StatusCode::FORBIDDEN);
        };
        let session = lease.session();
        let body = json!({"name":session.name(), "provider":lease.provider(), "expires":rfc3339(session.expires()), "csrf":session.csrf(), "remainingMs":remaining_ms(session.expires()), "maximumLifetimeMs":SESSION_LIFETIME.as_millis() as u64});
        json(StatusCode::OK, body)
    }

    fn logout(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let Some((form, lease)) = self.form_session(authorized) else {
            return response(StatusCode::FORBIDDEN);
        };
        {
            // Checking the originating login and revoking its scope is one
            // transaction; an already revoked lease cannot revoke sibling logins.
            let mut state = lock(&self.sessions.0);
            if !state.contains(&lease.session) || !lease.is_active() {
                return response(StatusCode::FORBIDDEN);
            }
            if value(&form, "scope") == "all" {
                state.revoke_subject(lease.session().subject());
            } else {
                state.remove(&lease.session.0.hash);
            }
        }
        self.log.count(Counter::Logout);
        let signed_out = query_url(AuthRoute::Login.path(), &[("reason", "signed_out")]);
        let mut result = redirect(authorized.request().method(), &signed_out);
        for name in ["__Host-gm_session", "__Host-gm_login", "__Host-gm_csrf"] {
            clear_cookie(&mut result, name, "Strict");
        }
        result
    }

    fn approval_page(&self, authorized: &AuthorizedRequest<Bytes>, browser: bool) -> Response<Bytes> {
        let request = authorized.request();
        let query = query_pairs(request);
        let challenge = value(&query, "challenge");
        let Some(valid) = valid_challenge(challenge) else {
            return refusal_page(false);
        };
        // As in Go, the CLI page redirects before it reads the client's address.
        if !browser && let Some(destination) = self.sessions.browser_approval_redirect(challenge) {
            return redirect(request.method(), &destination);
        }
        let client = self.client(authorized);
        // Public approval pages may inspect an ambient cookie, but never treat a
        // request carrying Authorization as a cookie-authenticated request.
        let bearer = text(request.headers(), "authorization") != Some("");
        let session = cookie(request.headers(), "__Host-gm_session").filter(|_| !bearer);
        let session = session.and_then(|token| self.sessions.lookup(token));
        let origin = value(&query, "client_origin");
        let login = query_url(AuthRoute::Login.path(), &[("challenge", challenge)]);
        let sign_in = || redirect(request.method(), &login);
        let approval = if browser {
            if !super::secure_browser_origin(origin) {
                return refusal_page(false);
            }
            let Some(client) = client else {
                return refusal_page(true);
            };
            if self.sessions.browser_approval_redirect(challenge).is_none()
                && !self.attempts.allow(Budget::BrowserApproval, client)
            {
                return refusal_page(true);
            }
            self.sessions
                .begin_browser_approval(&valid, origin, session.as_ref(), client)
        } else {
            let Some(session) = &session else {
                return sign_in();
            };
            let Some(client) = client else {
                return refusal_page(true);
            };
            self.sessions.begin_cli_approval(session, &valid, client)
        };
        match approval {
            Ok(view) => {
                let Some(session) = session else {
                    if browser
                        && text(request.headers(), "sec-fetch-site") == Some("cross-site")
                        && text(request.headers(), "sec-fetch-mode") == Some("navigate")
                        && text(request.headers(), "sec-fetch-dest") == Some("document")
                    {
                        return html(StatusCode::OK, pages::continue_page(challenge, true));
                    }
                    return sign_in();
                };
                let origin = view.browser_origin.as_deref().unwrap_or_default();
                let page = pages::approval_page(&view.code, session.session().csrf(), challenge, origin);
                html(StatusCode::OK, page)
            }
            Err(ApprovalError::GrantCapacity) => capacity_page(),
            Err(ApprovalError::Capacity) => {
                if !browser {
                    self.log.count(Counter::Capacity);
                }
                refusal_page(true)
            }
            Err(_) => refusal_page(false),
        }
    }

    fn approve(&self, authorized: &AuthorizedRequest<Bytes>, kind: ApprovalKind) -> Response<Bytes> {
        let Some((form, lease)) = self.form_session(authorized) else {
            return response(StatusCode::FORBIDDEN);
        };
        match self.sessions.approve(&lease.session, value(&form, "challenge"), kind) {
            Ok(()) => {
                self.log.count(Counter::CliApproval);
                html(StatusCode::OK, pages::done_page(kind == ApprovalKind::Browser))
            }
            Err(ApprovalError::GrantCapacity) => capacity_page(),
            Err(_) => response(StatusCode::FORBIDDEN),
        }
    }

    fn exchange(&self, request: &Request<Bytes>, browser: bool) -> Response<Bytes> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(default)]
            verifier: String,
        }
        let origin = text(request.headers(), "origin").unwrap_or_default();
        if browser && !super::secure_browser_origin(origin) {
            return response(StatusCode::FORBIDDEN);
        }
        let payload = (request.body().len() <= FORM_BYTES)
            .then(|| serde_json::from_slice::<Payload>(request.body()).ok())
            .flatten();
        let exchange = match payload {
            Some(payload) if browser => self.sessions.exchange_browser(&payload.verifier, origin),
            Some(payload) => self.sessions.exchange_cli(&payload.verifier),
            None if browser => Err(ExchangeError::InvalidVerifier),
            None => Ok(Exchange::Pending),
        };
        let mut result = match exchange {
            Ok(Exchange::Pending) => json(StatusCode::ACCEPTED, json!({"status":"pending"})),
            Ok(Exchange::Issued { token, lease }) => {
                let expires = lease.session().expires();
                if browser {
                    let body = json!({"token":token, "expires":unix_ms(expires), "remainingMs":remaining_ms(expires), "maximumLifetimeMs":SESSION_LIFETIME.as_millis() as u64});
                    json(StatusCode::OK, body)
                } else {
                    json(StatusCode::OK, json!({"token":token, "expires":rfc3339(expires)}))
                }
            }
            Err(ExchangeError::GrantCapacity) => response(StatusCode::TOO_MANY_REQUESTS),
            Err(ExchangeError::RandomUnavailable) => response(StatusCode::SERVICE_UNAVAILABLE),
            Err(_) => response(StatusCode::FORBIDDEN),
        };
        if browser {
            Access::Bearer(&HeaderValue::from_str(origin).expect("validated origin"))
                .apply_response(result.headers_mut());
        }
        result
    }

    fn ticket(&self, authorized: &AuthorizedRequest<Bytes>, kind: Kind) -> Response<Bytes> {
        let request = authorized.request();
        let Some(lease) = principal(authorized) else {
            return error_response(StatusCode::FORBIDDEN);
        };
        let (query, public) = (query_pairs(request), self.policy.public_origin());
        let origin = text(request.headers(), "origin").unwrap_or_default();
        let target = value(&query, "target");
        match self.sessions.mint_ticket(lease, public, target, origin, kind) {
            Ok(ticket) => json(
                StatusCode::OK,
                json!({"token":ticket.token, "expires":unix_ms(ticket.expires)}),
            ),
            Err(TicketError::InvalidTarget) => error_response(StatusCode::BAD_REQUEST),
            Err(TicketError::NoSession) => error_response(StatusCode::FORBIDDEN),
            Err(TicketError::Capacity) => {
                let mut result = error_response(StatusCode::TOO_MANY_REQUESTS);
                result
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                result
            }
            Err(TicketError::RandomUnavailable) => error_response(StatusCode::SERVICE_UNAVAILABLE),
        }
    }

    fn client(&self, authorized: &AuthorizedRequest<Bytes>) -> Option<IpAddr> {
        let (headers, peer) = (authorized.request().headers(), authorized.connection().peer);
        self.policy.client_address(headers, peer)
    }

    /// The posted form and the cookie login whose CSRF proof it carries from the public origin.
    fn form_session<'a>(&self, authorized: &'a AuthorizedRequest<Bytes>) -> Option<(Form, &'a AuthLease)> {
        let form = form(authorized.request()).ok()?;
        let lease = principal(authorized)?;
        (lease.is_active()
            && !lease.is_bearer()
            && text(authorized.request().headers(), "origin") == Some(self.policy.public_origin())
            && constant_equal(lease.session().csrf(), value(&form, "csrf")))
        .then_some((form, lease))
    }
}

fn principal(authorized: &AuthorizedRequest<Bytes>) -> Option<&AuthLease> {
    match authorized.authorization() {
        Authorization::Authenticated(lease) => Some(lease),
        _ => None,
    }
}
fn response(status: StatusCode) -> Response<Bytes> {
    secured(status, Response::new(Bytes::new()))
}
/// Go's auth page headers come first; `response`'s own follow, replacing any of the same name.
fn secured(status: StatusCode, mut response: Response<Bytes>) -> Response<Bytes> {
    let mut headers = pages::security_headers(None).expect("static auth CSP");
    pages::harden(&mut headers, true);
    for (name, value) in response.headers() {
        headers.insert(name, value.clone());
    }
    *response.headers_mut() = headers;
    *response.status_mut() = status;
    response
}
fn html(status: StatusCode, body: String) -> Response<Bytes> {
    let mut response = response(status);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    *response.body_mut() = body.into();
    response
}
fn json(status: StatusCode, value: serde_json::Value) -> Response<Bytes> {
    secured(status, json_response(value.to_string()))
}
fn error_response(status: StatusCode) -> Response<Bytes> {
    secured(status, text_response(status))
}
/// Go's http.Redirect, under the auth pages' headers.
fn redirect(method: &Method, destination: &str) -> Response<Bytes> {
    secured(
        StatusCode::SEE_OTHER,
        go_redirect(method, StatusCode::SEE_OTHER, destination),
    )
}
fn refusal_page(busy: bool) -> Response<Bytes> {
    html(StatusCode::FORBIDDEN, pages::refusal_page(busy))
}

fn capacity_page() -> Response<Bytes> {
    html(StatusCode::TOO_MANY_REQUESTS, pages::capacity_page())
}
fn query_url(path: &str, values: &[(&str, &str)]) -> String {
    format!(
        "{path}?{}",
        form_urlencoded::Serializer::new(String::new())
            .extend_pairs(values.iter().copied())
            .finish()
    )
}
fn value<'a>(values: &'a [(String, String)], key: &str) -> &'a str {
    values
        .iter()
        .find(|(name, _)| name == key)
        .map_or("", |(_, value)| value)
}
type Form = Vec<(String, String)>;

fn form(request: &Request<Bytes>) -> Result<Form, ()> {
    if request.body().len() > FORM_BYTES || media_type(request.headers()) != "application/x-www-form-urlencoded" {
        return Err(());
    }
    // Authentication credentials and CSRF proofs belong in the POST body,
    // never in URLs retained by browser history, proxies, or access logs.
    // Reject ambiguous fields rather than choosing one parser's precedence.
    let body = std::str::from_utf8(request.body()).map_err(|_| ())?;
    let mut names = std::collections::BTreeSet::new();
    body.split('&')
        .filter(|field| !field.is_empty())
        .map(|field| {
            let (key, value) = field.split_once('=').unwrap_or((field, ""));
            let key = unescape(key, true).filter(|key| !field.contains(';') && names.insert(key.clone()));
            Ok((key.ok_or(())?, unescape(value, true).ok_or(())?))
        })
        .collect()
}
/// Go's setCookie: only the CSRF cookie, which the pages' script reads, lacks HttpOnly.
fn set_cookie(response: &mut Response<Bytes>, name: &str, value: &str, expires: SystemTime, same_site: &str) {
    let age = expires.duration_since(SystemTime::now()).unwrap_or_default().as_secs();
    let http_only = if name == "__Host-gm_csrf" { "" } else { "; HttpOnly" };
    let date = httpdate::fmt_http_date(expires);
    let value =
        format!("{name}={value}; Path=/; Expires={date}; Max-Age={age}{http_only}; Secure; SameSite={same_site}");
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&value).expect("generated cookie"),
    );
}
fn clear_cookie(response: &mut Response<Bytes>, name: &str, same_site: &str) {
    set_cookie(response, name, "", UNIX_EPOCH + Duration::from_secs(1), same_site);
}
/// Go's issueSessionCookies.
fn session_cookies(response: &mut Response<Bytes>, token: &str, lease: &SessionLease) {
    let session = lease.session();
    set_cookie(response, "__Host-gm_session", token, session.expires(), "Strict");
    set_cookie(response, "__Host-gm_csrf", session.csrf(), session.expires(), "Strict");
    clear_cookie(response, "__Host-gm_login", "Strict");
}
fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}
fn remaining_ms(expires: SystemTime) -> u64 {
    expires
        .duration_since(SystemTime::now())
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::super::{
        oidc::tests::{discovered, query_fields, ready},
        policy::{Connection, Listener},
    };
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::net::Ipv4Addr;

    const PUBLIC: &str = "https://meter.example";
    const FORM: &str = "application/x-www-form-urlencoded";
    const HASH: &str =
        "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0";
    const PASSWORD: &str = "correct horse battery staple";

    impl Service {
        pub(crate) fn sessions(&self) -> &SessionStore {
            &self.sessions
        }
    }

    #[test]
    fn auth_forms_require_unambiguous_body_fields() {
        let request = |content_type, body: &'static str| {
            Request::builder()
                .method(Method::POST)
                .uri("/auth/password?password=url-secret&csrf=url-proof")
                .header(header::CONTENT_TYPE, content_type)
                .body(Bytes::from_static(body.as_bytes()))
                .unwrap()
        };
        let fields = form(&request(FORM, "challenge=example")).unwrap();
        assert_eq!(value(&fields, "password"), "");
        assert_eq!(value(&fields, "csrf"), "");
        for body in [
            "password=first&password=second",
            "p%61ssword=a&password=b",
            "csrf=a;b",
            "csrf=%zz",
        ] {
            assert!(form(&request(FORM, body)).is_err(), "{body}");
        }
        assert!(form(&request("text/plain", "password=secret")).is_err());
    }

    fn oidc_config() -> AuthConfig {
        AuthConfig {
            mode: AuthMode::Oidc,
            public_url: PUBLIC.into(),
            oidc_issuer: "https://identity.example".into(),
            oidc_client_id: "meter".into(),
            oidc_client_secret: "provider-secret".into(),
            oidc_allowed_groups: vec!["operators".into()],
            ..AuthConfig::default()
        }
    }

    fn oidc_service(oidc: Oidc) -> Service {
        let mut service = Service::new(&oidc_config(), vec![]).unwrap();
        service.oidc = Some(oidc);
        service
    }

    fn password_service(trusted: Vec<IpNet>) -> Service {
        let config = AuthConfig {
            mode: AuthMode::Password,
            public_url: PUBLIC.into(),
            password_hash: HASH.into(),
            ..AuthConfig::default()
        };
        Service::new(&config, trusted).unwrap()
    }

    async fn call(
        service: &Service,
        method: Method,
        path: &str,
        fields: &[(&str, &str)],
        body: String,
    ) -> Response<Bytes> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "meter.example");
        for (name, value) in fields {
            request = request.header(*name, *value);
        }
        let connection = Connection {
            peer: "192.0.2.1:1234".parse().unwrap(),
            tls: true,
            listener: Listener {
                ui: true,
                webtransport: true,
            },
        };
        let request = service
            .policy()
            .authorize(request.body(Bytes::from(body)).unwrap(), connection)
            .unwrap_or_else(|_| panic!("unexpected auth refusal for {path}"));
        service.handle(&request).await
    }

    async fn get(service: &Service, path: &str, headers: &[(&str, &str)]) -> Response<Bytes> {
        call(service, Method::GET, path, headers, String::new()).await
    }

    /// A form posted from the public origin, with `headers` besides.
    async fn post(service: &Service, path: &str, headers: &[(&str, &str)], fields: &[(&str, &str)]) -> Response<Bytes> {
        let headers = [&[("origin", PUBLIC), ("content-type", FORM)], headers].concat();
        let body = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields.iter().copied())
            .finish();
        call(service, Method::POST, path, &headers, body).await
    }

    /// The sign-in page's form nonce.
    async fn login_nonce(service: &Service) -> String {
        let page = get(service, "/login", &[]).await;
        assert_eq!(page.status(), StatusCode::OK);
        set_cookie_value(&page, "__Host-gm_login")
    }

    /// A sign-in with the provider, started by the page that set the login `nonce`.
    async fn start_oidc(service: &Service, nonce: &str) -> Response<Bytes> {
        let cookie = format!("__Host-gm_login={nonce}");
        let headers = [("cookie", cookie.as_str())];
        post(service, "/auth/oidc/start", &headers, &[("csrf", nonce)]).await
    }

    fn set_cookie_value(response: &Response<Bytes>, name: &str) -> String {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(|value| {
                value
                    .strip_prefix(&format!("{name}="))
                    .and_then(|value| value.split(';').next())
            })
            .expect("response cookie")
            .into()
    }

    fn location(response: &Response<Bytes>) -> &str {
        response.headers()[header::LOCATION].to_str().unwrap()
    }

    #[tokio::test]
    async fn hybrid_initialization_and_local_login_do_not_wait_for_stalled_provider() {
        use tokio::io::AsyncReadExt;
        let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = AuthConfig {
            mode: AuthMode::Hybrid,
            password_hash: HASH.into(),
            oidc_issuer: format!("https://localhost:{}", provider.local_addr().unwrap().port()),
            ..oidc_config()
        };
        let service = Arc::new(Service::new(&config, vec![]).unwrap());
        tokio::time::pause();
        let initialized = tokio::time::timeout(Duration::from_secs(1), service.initialize()).await;
        initialized.unwrap().unwrap();
        tokio::time::resume();
        let logging = {
            let service = service.clone();
            tokio::spawn(async move { service.security_log().await })
        };
        let (mut stalled, _) = provider.accept().await.unwrap();
        stalled.read_exact(&mut [0; 1]).await.unwrap();
        tokio::time::pause();
        let nonce = tokio::time::timeout(Duration::from_secs(1), login_nonce(&service)).await;
        let nonce = nonce.unwrap();
        tokio::time::resume();
        let cookie = format!("__Host-gm_login={nonce}");
        let fields = [("csrf", nonce.as_str()), ("password", PASSWORD)];
        let signed_in = post(&service, "/auth/password", &[("cookie", &cookie)], &fields).await;
        assert_eq!(signed_in.status(), StatusCode::SEE_OTHER);
        let token = set_cookie_value(&signed_in, "__Host-gm_session");
        assert!(service.sessions.lookup(&token).is_some());
        let line = service.log.window(&mut [0; Counter::COUNT]).unwrap();
        assert!(!line.contains(&token) && !line.contains("correct horse"));
        logging.abort();
        assert!(logging.await.unwrap_err().is_cancelled());
        let closed = stalled.read_to_end(&mut Vec::new()).await;
        let reset = |error: std::io::Error| {
            use std::io::ErrorKind::{ConnectionReset, UnexpectedEof};
            matches!(error.kind(), ConnectionReset | UnexpectedEof)
        };
        assert!(closed.is_ok() || closed.is_err_and(reset));
    }

    #[tokio::test]
    async fn discovered_provider_serves_sign_in_and_authorization() {
        let service = oidc_service(ready());
        let provider_csp = |response: &Response<Bytes>| {
            let policy = response.headers()["content-security-policy"].to_str().unwrap();
            policy.contains("form-action 'self' https://identity.example")
        };
        let page = get(&service, "/login", &[]).await;
        let body = std::str::from_utf8(page.body()).unwrap();
        assert!(!body.contains("temporarily unavailable"));
        assert!(provider_csp(&page));
        let started = start_oidc(&service, &set_cookie_value(&page, "__Host-gm_login")).await;
        assert!(location(&started).starts_with("https://identity.example/authorize?"));
        assert!(provider_csp(&started));
    }

    #[tokio::test]
    async fn sign_in_pages_render_when_the_provider_names_no_usable_authorization_origin() {
        // No browser can post a sign-in form to port 0, so no page may name it in its form-action.
        let service = oidc_service(discovered("https://identity.example:0/authorize", true));
        let page = get(&service, "/login", &[]).await;
        assert_eq!(page.status(), StatusCode::OK);
        let policy = page.headers()["content-security-policy"].to_str().unwrap();
        assert!(policy.contains("form-action 'self';"), "{policy}");
        let started = start_oidc(&service, &set_cookie_value(&page, "__Host-gm_login")).await;
        let refused = location(&started);
        assert_eq!(
            refused, "/login?error=provider",
            "a sign-in started with no usable provider"
        );
    }

    #[tokio::test]
    async fn oidc_refusals_show_go_notices_and_keep_a_found_challenge() {
        let service = oidc_service(ready());
        let challenge = URL_SAFE_NO_PAD.encode([7; 32]);
        let nonce = login_nonce(&service).await;
        let login = format!("__Host-gm_login={nonce}");
        let fields = [("csrf", nonce.as_str()), ("challenge", &challenge)];
        let stale = post(&service, "/auth/oidc/start", &[], &fields).await;
        assert_eq!(location(&stale), format!("/login?challenge={challenge}&error=stale"));
        let started = post(&service, "/auth/oidc/start", &[("cookie", &login)], &fields).await;
        let state = query_fields(location(&started))["state"].clone();
        let browser = format!("__Host-gm_oidc={}", set_cookie_value(&started, "__Host-gm_oidc"));
        let callback = query_url(
            "/auth/oidc/callback",
            &[("state", &state), ("code", "code"), ("iss", "https://other.example")],
        );
        let foreign = get(&service, &callback, &[("cookie", &browser)]).await;
        assert_eq!(location(&foreign), format!("/login?challenge={challenge}&error=failed"));
        let transaction = started.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(transaction.contains("; Expires=") && transaction.ends_with("; HttpOnly; Secure; SameSite=Lax"));
        let cleared = foreign.headers()[header::SET_COOKIE].to_str().unwrap();
        let expired = "; Max-Age=0; HttpOnly; Secure; SameSite=Lax";
        assert!(cleared.ends_with(expired), "{cleared}");
        let cookieless = get(&service, &callback, &[]).await;
        assert_eq!(location(&cookieless), "/login?error=stale");
        // Where no transaction names a challenge, Go's refusal keeps the callback's own.
        let unknown = [("state", "unknown"), ("code", "code"), ("challenge", &challenge)];
        let unknown = query_url("/auth/oidc/callback", &unknown);
        let unknown = get(&service, &unknown, &[("cookie", &browser)]).await;
        assert_eq!(location(&unknown), format!("/login?challenge={challenge}&error=failed"));
    }

    #[tokio::test]
    async fn callback_replay_is_counted_without_recording_request_credentials() {
        let service = Service::new(&oidc_config(), vec![]).unwrap();
        let callback = query_url(
            "/auth/oidc/callback",
            &[("state", &"a".repeat(43)), ("code", "private-provider-code")],
        );
        let response = get(&service, &callback, &[("cookie", "__Host-gm_oidc=private-cookie")]).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(location(&response), "/login?error=failed");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert_eq!(response.body(), "<a href=\"/login?error=failed\">See Other</a>.\n\n");
        let line = service.log.window(&mut [0; Counter::COUNT]).unwrap();
        assert!(line.contains("oidc-failure=1 group-denial=0 replay-expiry=1"), "{line}");
        assert!(!line.contains("private-") && !line.contains("provider-secret"));
    }

    #[tokio::test]
    async fn a_device_cookie_signs_in_past_the_global_ceiling_and_a_full_address_table() {
        use hmac::{Hmac, KeyInit, Mac};
        for full_table in [false, true] {
            let service = password_service(vec!["192.0.2.1/32".parse().unwrap()]);
            let sign_in = async |address: &str, device: Option<&str>| {
                let nonce = login_nonce(&service).await;
                let device = device.unwrap_or_default();
                let cookies = format!("__Host-gm_login={nonce}; __Host-gm_device={device}");
                let headers = [("cookie", cookies.as_str()), ("x-real-ip", address)];
                let fields = [("csrf", nonce.as_str()), ("password", PASSWORD)];
                let response = post(&service, "/auth/password", &headers, &fields).await;
                (location(&response) == "/").then_some(response)
            };
            let first = sign_in("198.51.100.7", None).await.expect("first sign-in");
            let issued = first.headers().get_all(header::SET_COOKIE).iter().next_back();
            let issued = issued.unwrap().to_str().unwrap();
            let parts = [
                "__Host-gm_device=",
                "; Max-Age=259199",
                "; Secure",
                "; SameSite=Strict",
                "; HttpOnly",
            ];
            let attributes = parts.iter().all(|part| issued.contains(part));
            assert!(issued.starts_with(parts[0]) && attributes, "{issued}");
            let device = set_cookie_value(&first, "__Host-gm_device");
            if full_table {
                for address in 0..2047 {
                    let address = Ipv4Addr::from(0x0a00_0000 + address).into();
                    assert!(service.attempts.allow(Budget::Password, address));
                }
            } else {
                for _ in 0..60 {
                    service.attempts.note_failed_password();
                }
            }
            let unknown = sign_in("203.0.113.9", None).await;
            assert!(unknown.is_none(), "an unknown client passed the shared bounds");
            let known = sign_in("192.0.2.77", Some(&device)).await;
            assert!(known.is_some(), "the known device was locked out");
            let mut forged = URL_SAFE_NO_PAD.decode(&device).unwrap();
            forged[39] ^= 1;
            let past = SystemTime::now() - Duration::from_secs(60);
            let past = past.duration_since(UNIX_EPOCH).unwrap().as_secs().to_be_bytes();
            let mut tag = Hmac::<sha2::Sha256>::new_from_slice(HASH.as_bytes()).unwrap();
            tag.update(&past);
            let expired = [past.as_slice(), &tag.finalize().into_bytes()].concat();
            for value in [forged, expired] {
                let device = URL_SAFE_NO_PAD.encode(value);
                assert!(sign_in("192.0.2.78", Some(&device)).await.is_none());
            }
        }
    }

    #[tokio::test]
    async fn browser_token_exchange_refuses_an_insecure_origin() {
        let service = Service::new(&oidc_config(), vec![]).unwrap();
        let body = json!({"verifier": "v".repeat(43)}).to_string();
        let headers = [("origin", "http://client.example")];
        let refused = call(&service, Method::POST, "/auth/browser/token", &headers, body).await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn logout_revokes_the_login_or_with_scope_all_every_login_of_its_subject() {
        let service = password_service(vec![]);
        for scope in ["", "all"] {
            let (token, current) = service.sessions.create("subject", "name", "local", None).unwrap();
            let (_, sibling) = service.sessions.create("subject", "name", "local", None).unwrap();
            let (_, other) = service.sessions.create("other", "name", "local", None).unwrap();
            let cookie = format!("__Host-gm_session={token}");
            let fields = [("csrf", current.session().csrf()), ("scope", scope)];
            let logged_out = post(&service, "/auth/logout", &[("cookie", &cookie)], &fields).await;
            let line = service.log.window(&mut [0; Counter::COUNT]).unwrap();
            assert!(!line.contains(&token) && !line.contains(current.session().csrf()));
            assert_eq!(logged_out.status(), StatusCode::SEE_OTHER);
            let cleared = logged_out.headers().get_all(header::SET_COOKIE);
            let cleared: Vec<_> = cleared.iter().map(|value| value.to_str().unwrap()).collect();
            let expired = "=; Path=/; Expires=Thu, 01 Jan 1970 00:00:01 GMT; Max-Age=0";
            assert_eq!(
                cleared,
                [
                    format!("__Host-gm_session{expired}; HttpOnly; Secure; SameSite=Strict"),
                    format!("__Host-gm_login{expired}; HttpOnly; Secure; SameSite=Strict"),
                    format!("__Host-gm_csrf{expired}; Secure; SameSite=Strict"),
                ]
            );
            assert!(!current.is_active() && other.is_active());
            assert_eq!(sibling.is_active(), scope.is_empty(), "scope {scope:?}");
        }
    }
}

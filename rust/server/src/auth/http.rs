//! HTTP authentication controller. Requests arrive only after policy authorization.
use super::{
    ApprovalError, ApprovalKind, AuthLease, AuthRoute, Exchange, ExchangeError, SESSION_LIFETIME, SessionLease,
    SessionStore, TicketError,
    logging::{Counter, SecurityLog},
    oidc::Oidc,
    pages::{self, LoginPage},
    password_login::{PasswordAttempt, PasswordLogin, check_csrf},
    policy::{Authorization, AuthorizedRequest, Policy, constant_equal, cookie},
    rate::{AttemptLimiter, Budget},
    reason::Reason,
    session::random_token,
    ticket::unescape,
    valid_challenge,
};
use crate::{
    config::{AuthConfig, AuthMode, ConfigError},
    cors::Access,
    http::response::{json_response, query_pairs, redirect_link, text_response},
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
            mode: config.mode,
            sessions,
            attempts,
            log,
        };
        let mode = match config.mode {
            AuthMode::Off => "off",
            AuthMode::Password => "password",
            AuthMode::Oidc => "oidc",
            AuthMode::Hybrid => "hybrid",
        };
        crate::log!(
            "[gm:auth] mode={mode} origin={} provider={} issuer={} allowed-groups={} session-lifetime={}",
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
        let mut result = match AuthRoute::lookup(method, path) {
            Some(AuthRoute::Login) => self.login_page(request).await,
            Some(AuthRoute::OidcStart) => self.oidc_start(authorized).await,
            Some(AuthRoute::OidcCallback) => self.oidc_callback(authorized).await,
            Some(AuthRoute::Password) => self.password_login(authorized).await,
            Some(AuthRoute::Session) => self.session_info(authorized),
            Some(AuthRoute::Logout) => self.logout(authorized),
            Some(AuthRoute::CliPage) => self.approval_page(authorized, false),
            Some(AuthRoute::BrowserPage) => self.approval_page(authorized, true),
            Some(AuthRoute::CliApprove) => self.approve(authorized, false),
            Some(AuthRoute::BrowserApprove) => self.approve(authorized, true),
            Some(AuthRoute::CliToken) => self.exchange(request, false),
            Some(AuthRoute::BrowserToken) => self.exchange(request, true),
            None => match route::lookup(path) {
                Some(Route::WtSession) if request.method() == Method::POST => {
                    self.ticket(authorized, Kind::WebTransport)
                }
                Some(Route::WsSession) if request.method() == Method::POST => self.ticket(authorized, Kind::WebSocket),
                _ => error_response(StatusCode::NOT_FOUND),
            },
        };
        // As Go's http.Redirect, a GET's or HEAD's redirect is HTML, and a GET's also links its destination.
        if result.status() == StatusCode::SEE_OTHER && request.method() == Method::HEAD {
            result.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
        }
        if result.status() == StatusCode::SEE_OTHER && request.method() == Method::GET {
            let location = result.headers()[header::LOCATION].to_str().unwrap_or_default();
            *result.body_mut() = redirect_link(StatusCode::SEE_OTHER, location).into();
            result.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
        }
        result
    }

    async fn login_page(&self, request: &Request<Bytes>) -> Response<Bytes> {
        let provider = self.oidc.as_ref().and_then(|oidc| oidc.ready());
        let Ok(nonce) = random_token::<32>() else {
            return response(StatusCode::SERVICE_UNAVAILABLE);
        };
        let query = query_pairs(request);
        let challenge = value(&query, "challenge");
        let challenge = if valid_challenge(challenge) { challenge } else { "" };
        let notice = match value(&query, "error") {
            "" => "",
            value @ ("provider" | "busy" | "stale" | "throttled" | "password") => value,
            _ => "failed",
        };
        let status = match value(&query, "reason") {
            value @ ("expired" | "renew" | "signed_out") => value,
            _ => "",
        };
        let mut result = html(
            StatusCode::OK,
            LoginPage {
                csrf: &nonce,
                provider: self.oidc.as_ref().map_or("", |oidc| oidc.name()),
                challenge,
                password: self.password.is_some(),
                oidc: self.oidc.is_some(),
                oidc_ready: provider.is_some(),
                notice,
                status,
            }
            .render(),
        );
        if let Some(provider) = provider {
            result.headers_mut().extend(provider.page_headers.clone());
        }
        set_cookie(
            &mut result,
            "__Host-gm_login",
            &nonce,
            SystemTime::now() + LOGIN_NONCE_LIFETIME,
            "Strict",
        );
        result
    }

    async fn password_login(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let request = authorized.request();
        let Some(password) = &self.password else {
            return error_response(StatusCode::NOT_FOUND);
        };
        let Ok(form) = form(request) else {
            return self.rejected(Reason::MalformedForm, "");
        };
        let challenge = value(&form, "challenge");
        let result = password
            .attempt(PasswordAttempt {
                client: self
                    .policy
                    .client_address(request.headers(), authorized.connection().peer),
                origin: text(request, "origin"),
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
                let destination = if valid_challenge(challenge) {
                    query_url(AuthRoute::CliPage.path(), &[("challenge", challenge)])
                } else {
                    "/".into()
                };
                let mut result = redirect(&destination);
                session_cookies(&mut result, &token, &session);
                let (device, expires) = password.device_cookie(SystemTime::now());
                set_cookie(&mut result, "__Host-gm_device", &device, expires, "Strict");
                result
            }
            Err(reason) => self.rejected(reason, challenge),
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
            return self.oidc_rejected(Reason::ProviderNotReady, challenge);
        }
        let Ok(form) = &form else {
            return self.oidc_rejected(Reason::MalformedForm, "");
        };
        let origin = text(request, "origin");
        let nonce = cookie(request.headers(), "__Host-gm_login");
        if let Err(reason) = check_csrf(self.policy.public_origin(), origin, nonce, value(form, "csrf")) {
            return self.oidc_rejected(reason, challenge);
        }
        let address = self
            .policy
            .client_address(request.headers(), authorized.connection().peer);
        let Some(address) = address.filter(|&address| self.attempts.allow(Budget::OidcStart, address)) else {
            return self.oidc_rejected(Reason::Throttled, challenge);
        };
        let prior = cookie(request.headers(), "__Host-gm_session").and_then(|token| self.sessions.lookup(token));
        let stored = if valid_challenge(challenge) { challenge } else { "" };
        match oidc.start(address, stored.to_owned(), prior).await {
            Ok(started) => {
                let mut result = redirect(&started.url);
                result.headers_mut().extend(started.provider.page_headers.clone());
                let expires = SystemTime::now() + super::oidc::TRANSACTION_LIFETIME;
                set_cookie(&mut result, "__Host-gm_oidc", &started.browser, expires, "Lax");
                result
            }
            Err(reason) => self.oidc_rejected(reason, challenge),
        }
    }

    async fn oidc_callback(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let Some(oidc) = &self.oidc else {
            return error_response(StatusCode::NOT_FOUND);
        };
        let mut result = self
            .oidc_login(oidc, authorized)
            .await
            .unwrap_or_else(|(reason, challenge)| self.oidc_rejected(reason, &challenge));
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
        let address = self
            .policy
            .client_address(request.headers(), authorized.connection().peer);
        if !address.is_some_and(|address| self.attempts.allow(Budget::OidcExchange, address)) {
            return Err(fail(Reason::ExchangeRateLimited));
        }
        let identity = oidc.complete(&tx, code).await.map_err(fail)?;
        let (token, session) = self
            .sessions
            .create(&identity.subject, &identity.name, oidc.name(), None)
            .map_err(|_| fail(Reason::SessionCapacity))?;
        if let Some(prior) = &tx.prior {
            self.sessions.revoke(prior);
        }
        let mut result = html(StatusCode::OK, pages::continue_page(&tx.challenge, false));
        session_cookies(&mut result, &token, &session);
        self.log.count(Counter::Oidc);
        Ok(result)
    }

    fn oidc_rejected(&self, reason: Reason, challenge: &str) -> Response<Bytes> {
        self.log.count(Counter::OidcFailure);
        self.rejected(reason, challenge)
    }

    fn rejected(&self, reason: Reason, challenge: &str) -> Response<Bytes> {
        self.log.refused(reason);
        let mut fields = Vec::new();
        if valid_challenge(challenge) {
            fields.push(("challenge", challenge));
        }
        fields.push(("error", reason.notice()));
        redirect(&query_url(AuthRoute::Login.path(), &fields))
    }

    fn session_info(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let Some(lease) = principal(authorized) else {
            return response(StatusCode::FORBIDDEN);
        };
        let session = lease.session();
        json(
            StatusCode::OK,
            json!({"name":session.name(), "provider":lease.provider(), "expires":rfc3339(session.expires()), "csrf":session.csrf(), "remainingMs":remaining_ms(session.expires()), "maximumLifetimeMs":SESSION_LIFETIME.as_millis() as u64}),
        )
    }

    fn logout(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let Ok(form) = form(authorized.request()) else {
            return response(StatusCode::FORBIDDEN);
        };
        let Some(lease) = self.form_session(authorized, &form) else {
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
        let mut result = redirect(&query_url(AuthRoute::Login.path(), &[("reason", "signed_out")]));
        for name in ["__Host-gm_session", "__Host-gm_login", "__Host-gm_csrf"] {
            clear_cookie(&mut result, name, "Strict");
        }
        result
    }

    fn approval_page(&self, authorized: &AuthorizedRequest<Bytes>, browser: bool) -> Response<Bytes> {
        let request = authorized.request();
        let query = query_pairs(request);
        let challenge = value(&query, "challenge");
        if !valid_challenge(challenge) {
            return response(StatusCode::FORBIDDEN);
        }
        // As in Go, the CLI page redirects before it reads the client's address.
        if !browser && let Some(destination) = self.sessions.browser_approval_redirect(challenge) {
            return redirect(&destination);
        }
        let client = self
            .policy
            .client_address(request.headers(), authorized.connection().peer);
        // Public approval pages may inspect an ambient cookie, but never treat a
        // request carrying Authorization as a cookie-authenticated request.
        let session = if request
            .headers()
            .get(header::AUTHORIZATION)
            .is_some_and(|value| !value.is_empty())
        {
            None
        } else {
            cookie(request.headers(), "__Host-gm_session").and_then(|token| self.sessions.lookup(token))
        };
        let origin = value(&query, "client_origin");
        let approval = if browser {
            let Some(client) = client.filter(|_| super::secure_browser_origin(origin)) else {
                return response(StatusCode::FORBIDDEN);
            };
            if self.sessions.browser_approval_redirect(challenge).is_none()
                && !self.attempts.allow(Budget::BrowserApproval, client)
            {
                return response(StatusCode::FORBIDDEN);
            }
            self.sessions
                .begin_browser_approval(challenge, origin, session.as_ref(), client)
        } else {
            let Some(session) = &session else {
                return redirect(&query_url(AuthRoute::Login.path(), &[("challenge", challenge)]));
            };
            let Some(client) = client else {
                return response(StatusCode::FORBIDDEN);
            };
            self.sessions.begin_cli_approval(session, challenge, client)
        };
        match approval {
            Ok(view) => {
                let Some(session) = session else {
                    if browser
                        && text(request, "sec-fetch-site") == "cross-site"
                        && text(request, "sec-fetch-mode") == "navigate"
                        && text(request, "sec-fetch-dest") == "document"
                    {
                        return html(StatusCode::OK, pages::continue_page(challenge, true));
                    }
                    return redirect(&query_url(AuthRoute::Login.path(), &[("challenge", challenge)]));
                };
                let origin = view.browser_origin.as_deref().unwrap_or_default();
                html(
                    StatusCode::OK,
                    pages::approval_page(&view.code, session.session().csrf(), challenge, origin),
                )
            }
            Err(ApprovalError::GrantCapacity) => capacity_page(),
            Err(ApprovalError::Capacity) => {
                if !browser {
                    self.log.count(Counter::Capacity);
                }
                response(StatusCode::FORBIDDEN)
            }
            Err(_) => response(StatusCode::FORBIDDEN),
        }
    }

    fn approve(&self, authorized: &AuthorizedRequest<Bytes>, browser: bool) -> Response<Bytes> {
        let Ok(form) = form(authorized.request()) else {
            return response(StatusCode::FORBIDDEN);
        };
        let Some(lease) = self.form_session(authorized, &form) else {
            return response(StatusCode::FORBIDDEN);
        };
        let challenge = value(&form, "challenge");
        match self.sessions.approve(
            &lease.session,
            challenge,
            if browser {
                ApprovalKind::Browser
            } else {
                ApprovalKind::Cli
            },
        ) {
            Ok(()) => {
                self.log.count(Counter::CliApproval);
                html(StatusCode::OK, pages::done_page(browser))
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
        let origin = text(request, "origin");
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
                    json(
                        StatusCode::OK,
                        json!({"token":token, "expires":unix_ms(expires), "remainingMs":remaining_ms(expires), "maximumLifetimeMs":SESSION_LIFETIME.as_millis() as u64}),
                    )
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
        let query = query_pairs(request);
        match self.sessions.mint_ticket(
            lease,
            self.policy.public_origin(),
            value(&query, "target"),
            text(request, "origin"),
            kind,
        ) {
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

    fn form_session<'a>(
        &self,
        authorized: &'a AuthorizedRequest<Bytes>,
        form: &[(String, String)],
    ) -> Option<&'a AuthLease> {
        let lease = principal(authorized)?;
        (lease.is_active()
            && !lease.is_bearer()
            && text(authorized.request(), "origin") == self.policy.public_origin()
            && constant_equal(lease.session().csrf(), value(form, "csrf")))
        .then_some(lease)
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
fn redirect(destination: &str) -> Response<Bytes> {
    let mut response = response(StatusCode::SEE_OTHER);
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(destination).expect("encoded redirect"),
    );
    response
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
fn text<'a>(request: &'a Request<Bytes>, name: &str) -> &'a str {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}
fn form(request: &Request<Bytes>) -> Result<Vec<(String, String)>, ()> {
    if request.body().len() > FORM_BYTES
        || !text(request, "content-type")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/x-www-form-urlencoded")
    {
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
    use super::super::policy::{Connection, Listener};
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::net::Ipv4Addr;

    impl Service {
        pub(crate) fn sessions(&self) -> &SessionStore {
            &self.sessions
        }
    }

    #[test]
    fn auth_forms_require_unambiguous_body_fields() {
        const FORM: &str = "application/x-www-form-urlencoded";
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

    fn connection() -> Connection {
        Connection {
            peer: "192.0.2.1:1234".parse().unwrap(),
            tls: true,
            listener: Listener {
                ui: true,
                webtransport: true,
            },
        }
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
        let request = service
            .policy()
            .authorize(request.body(Bytes::from(body)).unwrap(), connection())
            .unwrap_or_else(|_| panic!("unexpected auth refusal for {path}"));
        service.handle(&request).await
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
    fn encoded(values: &[(&str, &str)]) -> String {
        form_urlencoded::Serializer::new(String::new())
            .extend_pairs(values.iter().copied())
            .finish()
    }

    #[tokio::test]
    async fn hybrid_initialization_and_local_login_do_not_wait_for_stalled_provider() {
        use tokio::io::AsyncReadExt;
        const PUBLIC: &str = "https://meter.example";
        let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let service = Arc::new(Service::new(&AuthConfig {
            mode: AuthMode::Hybrid,
            public_url: PUBLIC.into(),
            password_hash: "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0".into(),
            oidc_issuer: format!("https://localhost:{}", provider.local_addr().unwrap().port()),
            oidc_client_id: "meter".into(),
            oidc_client_secret: "provider-secret".into(),
            oidc_allowed_groups: vec!["operators".into()],
            ..AuthConfig::default()
        }, vec![]).unwrap());
        tokio::time::pause();
        tokio::time::timeout(Duration::from_secs(1), service.initialize())
            .await
            .unwrap()
            .unwrap();
        tokio::time::resume();
        let logging = {
            let service = service.clone();
            tokio::spawn(async move { service.security_log().await })
        };
        let (mut stalled, _) = provider.accept().await.unwrap();
        let mut hello = [0; 1];
        stalled.read_exact(&mut hello).await.unwrap();
        tokio::time::pause();
        let login = tokio::time::timeout(
            Duration::from_secs(1),
            call(&service, Method::GET, "/login", &[], String::new()),
        )
        .await
        .unwrap();
        tokio::time::resume();
        assert_eq!(login.status(), StatusCode::OK);
        let nonce = set_cookie_value(&login, "__Host-gm_login");
        let cookie = format!("__Host-gm_login={nonce}");
        let signed_in = call(
            &service,
            Method::POST,
            "/auth/password",
            &[
                ("cookie", &cookie),
                ("origin", PUBLIC),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            encoded(&[("csrf", &nonce), ("password", "correct horse battery staple")]),
        )
        .await;
        assert_eq!(signed_in.status(), StatusCode::SEE_OTHER);
        assert!(
            service
                .sessions()
                .lookup(&set_cookie_value(&signed_in, "__Host-gm_session"))
                .is_some()
        );
        logging.abort();
        assert!(logging.await.unwrap_err().is_cancelled());
        let closed = stalled.read_to_end(&mut Vec::new()).await;
        assert!(
            closed.is_ok()
                || closed.is_err_and(|error| matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::UnexpectedEof
                ))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_logins_always_see_the_discovered_provider() {
        let mut service = Service::new(
            &AuthConfig {
                mode: AuthMode::Oidc,
                public_url: "https://meter.example".into(),
                oidc_issuer: "https://identity.example".into(),
                oidc_client_id: "meter".into(),
                oidc_client_secret: "secret".into(),
                oidc_allowed_groups: vec!["operators".into()],
                ..AuthConfig::default()
            },
            vec![],
        )
        .unwrap();
        service.oidc = Some(super::super::oidc::tests::ready());
        let service = Arc::new(service);
        let provider_csp = |response: &Response<Bytes>| {
            response.headers().get("content-security-policy").is_some_and(|csp| {
                csp.to_str()
                    .unwrap()
                    .contains("form-action 'self' https://identity.example")
            })
        };
        let mut logins = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let service = service.clone();
            logins.spawn(async move {
                for attempt in 0..500 {
                    let page = call(&service, Method::GET, "/login", &[], String::new()).await;
                    let body = std::str::from_utf8(page.body()).unwrap();
                    assert!(!body.contains("temporarily unavailable"));
                    assert!(provider_csp(&page));
                    if attempt != 250 {
                        continue;
                    }
                    let nonce = set_cookie_value(&page, "__Host-gm_login");
                    let started = call(
                        &service,
                        Method::POST,
                        "/auth/oidc/start",
                        &[
                            ("cookie", &format!("__Host-gm_login={nonce}")),
                            ("origin", "https://meter.example"),
                            ("content-type", "application/x-www-form-urlencoded"),
                        ],
                        encoded(&[("csrf", &nonce)]),
                    )
                    .await;
                    assert!(
                        started.headers()[header::LOCATION]
                            .to_str()
                            .unwrap()
                            .starts_with("https://identity.example/authorize?")
                    );
                    assert!(provider_csp(&started));
                }
            });
        }
        while let Some(result) = logins.join_next().await {
            result.unwrap();
        }
    }

    #[tokio::test]
    async fn sign_in_pages_render_when_the_provider_names_no_usable_authorization_origin() {
        let mut service = Service::new(
            &AuthConfig {
                mode: AuthMode::Oidc,
                public_url: "https://meter.example".into(),
                oidc_issuer: "https://identity.example".into(),
                oidc_client_id: "meter".into(),
                oidc_client_secret: "secret".into(),
                oidc_allowed_groups: vec!["operators".into()],
                ..AuthConfig::default()
            },
            vec![],
        )
        .unwrap();
        // No browser can post a sign-in form to port 0, so no page may name it in its form-action.
        service.oidc = Some(super::super::oidc::tests::discovered(
            "https://identity.example:0/authorize",
            true,
        ));
        let page = call(&service, Method::GET, "/login", &[], String::new()).await;
        assert_eq!(page.status(), StatusCode::OK);
        let policy = page.headers()["content-security-policy"].to_str().unwrap();
        assert!(policy.contains("form-action 'self';"), "{policy}");
        let nonce = set_cookie_value(&page, "__Host-gm_login");
        let started = call(
            &service,
            Method::POST,
            "/auth/oidc/start",
            &[
                ("cookie", &format!("__Host-gm_login={nonce}")),
                ("origin", "https://meter.example"),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            encoded(&[("csrf", &nonce)]),
        )
        .await;
        assert_eq!(
            started.headers()[header::LOCATION],
            "/login?error=provider",
            "a sign-in started with no usable provider"
        );
    }

    #[tokio::test]
    async fn oidc_refusals_show_go_notices_and_keep_a_found_challenge() {
        let mut service = Service::new(
            &AuthConfig {
                mode: AuthMode::Oidc,
                public_url: "https://meter.example".into(),
                oidc_issuer: "https://identity.example".into(),
                oidc_client_id: "meter".into(),
                oidc_client_secret: "secret".into(),
                oidc_allowed_groups: vec!["operators".into()],
                ..AuthConfig::default()
            },
            vec![],
        )
        .unwrap();
        service.oidc = Some(super::super::oidc::tests::ready());
        let location = |response: &Response<Bytes>| response.headers()[header::LOCATION].to_str().unwrap().to_owned();
        let challenge = URL_SAFE_NO_PAD.encode([7; 32]);
        let nonce = set_cookie_value(
            &call(&service, Method::GET, "/login", &[], String::new()).await,
            "__Host-gm_login",
        );
        let login = format!("__Host-gm_login={nonce}");
        let mut start = vec![
            ("origin", "https://meter.example"),
            ("content-type", "application/x-www-form-urlencoded"),
        ];
        let form = || encoded(&[("csrf", &nonce), ("challenge", &challenge)]);
        let stale = call(&service, Method::POST, "/auth/oidc/start", &start, form()).await;
        assert_eq!(location(&stale), format!("/login?challenge={challenge}&error=stale"));
        start.push(("cookie", &login));
        let started = call(&service, Method::POST, "/auth/oidc/start", &start, form()).await;
        let state = super::super::oidc::tests::query_fields(&location(&started))["state"].clone();
        let browser = format!("__Host-gm_oidc={}", set_cookie_value(&started, "__Host-gm_oidc"));
        let callback = query_url(
            "/auth/oidc/callback",
            &[("state", &state), ("code", "code"), ("iss", "https://other.example")],
        );
        let foreign = call(&service, Method::GET, &callback, &[("cookie", &browser)], String::new()).await;
        assert_eq!(location(&foreign), format!("/login?challenge={challenge}&error=failed"));
        let transaction = started.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(transaction.contains("; Expires=") && transaction.ends_with("; HttpOnly; Secure; SameSite=Lax"));
        let cleared = foreign.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(
            cleared.ends_with("; Max-Age=0; HttpOnly; Secure; SameSite=Lax"),
            "{cleared}"
        );
        let cookieless = call(&service, Method::GET, &callback, &[], String::new()).await;
        assert_eq!(location(&cookieless), "/login?error=stale");
    }

    #[tokio::test]
    async fn callback_replay_is_counted_without_recording_request_credentials() {
        let service = Service::new(
            &AuthConfig {
                mode: AuthMode::Oidc,
                public_url: "https://meter.example".into(),
                oidc_issuer: "https://identity.example".into(),
                oidc_client_id: "client".into(),
                oidc_client_secret: "provider-secret".into(),
                oidc_allowed_groups: vec!["operators".into()],
                ..AuthConfig::default()
            },
            vec![],
        )
        .unwrap();
        let callback = query_url(
            "/auth/oidc/callback",
            &[("state", &"a".repeat(43)), ("code", "private-provider-code")],
        );
        let response = call(
            &service,
            Method::GET,
            &callback,
            &[("cookie", "__Host-gm_oidc=private-cookie")],
            String::new(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response.headers()[header::LOCATION].to_str().unwrap();
        assert_eq!(location, "/login?error=failed");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert_eq!(response.body(), "<a href=\"/login?error=failed\">See Other</a>.\n\n");
        let line = service.log.window(&mut [0; Counter::COUNT]).unwrap();
        assert!(line.contains("oidc-failure=1 group-denial=0 replay-expiry=1"), "{line}");
        assert!(!line.contains("private-"));
        assert!(!line.contains("provider-secret"));
    }

    #[tokio::test]
    async fn a_device_cookie_signs_in_past_the_global_ceiling_and_a_full_address_table() {
        use hmac::{Hmac, KeyInit, Mac};
        const PUBLIC: &str = "https://meter.example";
        const HASH: &str =
            "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0";
        for full_table in [false, true] {
            let config = AuthConfig {
                mode: AuthMode::Password,
                public_url: PUBLIC.into(),
                password_hash: HASH.into(),
                ..AuthConfig::default()
            };
            let service = Service::new(&config, vec!["192.0.2.1/32".parse().unwrap()]).unwrap();
            let sign_in = async |address: &str, device: Option<&str>| {
                let login = call(&service, Method::GET, "/login", &[], String::new()).await;
                let nonce = set_cookie_value(&login, "__Host-gm_login");
                let cookies = format!(
                    "__Host-gm_login={nonce}; __Host-gm_device={}",
                    device.unwrap_or_default()
                );
                let response = call(
                    &service,
                    Method::POST,
                    "/auth/password",
                    &[
                        ("cookie", &cookies),
                        ("origin", PUBLIC),
                        ("content-type", "application/x-www-form-urlencoded"),
                        ("x-real-ip", address),
                    ],
                    encoded(&[("csrf", &nonce), ("password", "correct horse battery staple")]),
                )
                .await;
                (response.headers()[header::LOCATION] == "/").then_some(response)
            };
            let first = sign_in("198.51.100.7", None).await.expect("first sign-in");
            let issued = first
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .next_back()
                .unwrap()
                .to_str()
                .unwrap();
            assert!(
                issued.starts_with("__Host-gm_device=") && issued.contains("; Max-Age=259199"),
                "{issued}"
            );
            assert!(
                issued.contains("; Secure") && issued.contains("; SameSite=Strict") && issued.contains("; HttpOnly")
            );
            let device = set_cookie_value(&first, "__Host-gm_device");
            if full_table {
                for address in 0..2047 {
                    assert!(
                        service
                            .attempts
                            .allow(Budget::Password, Ipv4Addr::from(0x0a00_0000 + address).into())
                    );
                }
            } else {
                for _ in 0..60 {
                    service.attempts.note_failed_password();
                }
            }
            assert!(
                sign_in("203.0.113.9", None).await.is_none(),
                "an unknown client passed the shared bounds"
            );
            assert!(
                sign_in("192.0.2.77", Some(&device)).await.is_some(),
                "the known device was locked out"
            );
            let mut forged = URL_SAFE_NO_PAD.decode(&device).unwrap();
            forged[39] ^= 1;
            let past = (SystemTime::now() - Duration::from_secs(60))
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .to_be_bytes();
            let mut tag = Hmac::<sha2::Sha256>::new_from_slice(HASH.as_bytes()).unwrap();
            tag.update(&past);
            let expired = [past.as_slice(), &tag.finalize().into_bytes()].concat();
            for value in [forged, expired] {
                assert!(
                    sign_in("192.0.2.78", Some(&URL_SAFE_NO_PAD.encode(value)))
                        .await
                        .is_none()
                );
            }
        }
    }

    #[tokio::test]
    async fn password_ticket_logout_revokes_lease_and_records_counter_deltas() {
        const PUBLIC: &str = "https://meter.example";
        let service = Service::new(
            &AuthConfig {
                mode: AuthMode::Password,
                public_url: PUBLIC.into(),
                password_hash:
                    "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0"
                        .into(),
                ..AuthConfig::default()
            },
            vec![],
        )
        .unwrap();
        let login = call(&service, Method::GET, "/login", &[], String::new()).await;
        assert_eq!(login.status(), StatusCode::OK);
        let exchange = json!({"verifier": "v".repeat(43)}).to_string();
        let foreign = [("origin", "http://client.example")];
        let refused = call(&service, Method::POST, "/auth/browser/token", &foreign, exchange).await;
        assert_eq!(
            refused.status(),
            StatusCode::FORBIDDEN,
            "a browser token for an insecure origin"
        );
        let nonce = set_cookie_value(&login, "__Host-gm_login");
        let nonce_cookie = format!("__Host-gm_login={nonce}");
        let rejected = call(
            &service,
            Method::POST,
            "/auth/password",
            &[
                ("cookie", &nonce_cookie),
                ("origin", PUBLIC),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            encoded(&[("csrf", &nonce), ("password", "incorrect password")]),
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::SEE_OTHER);
        assert!(rejected.body().is_empty() && !rejected.headers().contains_key(header::CONTENT_TYPE));
        let signed_in = call(
            &service,
            Method::POST,
            "/auth/password",
            &[
                ("cookie", &nonce_cookie),
                ("origin", PUBLIC),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            encoded(&[("csrf", &nonce), ("password", "correct horse battery staple")]),
        )
        .await;
        assert_eq!(signed_in.status(), StatusCode::SEE_OTHER);
        let raw_session = set_cookie_value(&signed_in, "__Host-gm_session");
        let session_cookie = format!("__Host-gm_session={raw_session}");
        let csrf = set_cookie_value(&signed_in, "__Host-gm_csrf");
        let ticket = call(
            &service,
            Method::POST,
            &query_url("/wt/session", &[("target", "https://meter.example:8443/wt/ping")]),
            &[("origin", PUBLIC), ("cookie", &session_cookie), ("x-csrf-token", &csrf)],
            String::new(),
        )
        .await;
        assert_eq!(ticket.status(), StatusCode::OK);
        let ticket: serde_json::Value = serde_json::from_slice(ticket.body()).unwrap();
        let connect = Request::builder()
            .method(Method::CONNECT)
            .uri(query_url("/wt/ping", &[("token", ticket["token"].as_str().unwrap())]))
            .header(header::HOST, "meter.example:8443")
            .header(header::ORIGIN, PUBLIC)
            .body(Bytes::new())
            .unwrap();
        let connected = service
            .policy()
            .authorize(connect, connection())
            .unwrap_or_else(|_| panic!("ticket rejected"));
        let Authorization::Authenticated(active) = connected.authorization() else {
            panic!("missing active lease")
        };
        assert!(active.is_active());

        let logged_out = call(
            &service,
            Method::POST,
            "/auth/logout",
            &[
                ("cookie", &session_cookie),
                ("origin", PUBLIC),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            encoded(&[("csrf", &csrf)]),
        )
        .await;
        assert_eq!(logged_out.status(), StatusCode::SEE_OTHER);
        tokio::time::timeout(Duration::from_secs(1), active.ended())
            .await
            .unwrap();
        assert!(service.sessions().lookup(&raw_session).is_none());
        let mut last = [0; Counter::COUNT];
        let window = service.log.window(&mut last).unwrap();
        assert!(window.contains("local=1 oidc=0 invalid-password=1"), "{window}");
        assert!(window.contains("logout=1 cli-approval=0 capacity=0"), "{window}");
        assert!(service.log.window(&mut last).is_none());
        assert!(!window.contains(&raw_session));
        assert!(!window.contains("correct horse"));
    }

    #[tokio::test]
    async fn logout_revokes_the_login_or_with_scope_all_every_login_of_its_subject() {
        const PUBLIC: &str = "https://meter.example";
        let config = AuthConfig {
            mode: AuthMode::Password,
            public_url: PUBLIC.into(),
            password_hash:
                "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0"
                    .into(),
            ..AuthConfig::default()
        };
        let service = Service::new(&config, vec![]).unwrap();
        for scope in ["", "all"] {
            let (token, current) = service.sessions().create("subject", "name", "local", None).unwrap();
            let (_, sibling) = service.sessions().create("subject", "name", "local", None).unwrap();
            let (_, other) = service.sessions().create("other", "name", "local", None).unwrap();
            let cookie = format!("__Host-gm_session={token}");
            let headers = [
                ("cookie", cookie.as_str()),
                ("origin", PUBLIC),
                ("content-type", "application/x-www-form-urlencoded"),
            ];
            let form = encoded(&[("csrf", current.session().csrf()), ("scope", scope)]);
            let logged_out = call(&service, Method::POST, "/auth/logout", &headers, form).await;
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

    #[tokio::test]
    async fn the_cli_page_sends_a_signed_out_caller_to_sign_in_before_reading_its_address() {
        let config = AuthConfig {
            mode: AuthMode::Password,
            public_url: "https://meter.example".into(),
            password_hash:
                "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0"
                    .into(),
            ..AuthConfig::default()
        };
        // The peer is a trusted proxy whose forwarding names no usable client, as in Go's cliPage.
        let service = Service::new(&config, vec!["192.0.2.1/32".parse().unwrap()]).unwrap();
        let challenge = "A".repeat(43);
        let page = query_url("/auth/cli", &[("challenge", &challenge)]);
        let forwarded = [("x-forwarded-for", "unknown")];
        let response = call(&service, Method::GET, &page, &forwarded, String::new()).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()[header::LOCATION],
            format!("/login?challenge={challenge}")
        );
        let (token, _) = service.sessions().create("subject", "name", "local", None).unwrap();
        let cookie = format!("__Host-gm_session={token}");
        let signed_in = [forwarded[0], ("cookie", cookie.as_str())];
        let response = call(&service, Method::GET, &page, &signed_in, String::new()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}

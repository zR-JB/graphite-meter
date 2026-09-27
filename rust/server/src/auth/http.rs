//! HTTP authentication controller. Requests arrive only after policy authorization.
use super::{
    ApprovalError, ApprovalKind, AuthLease, Exchange, ExchangeError, SESSION_LIFETIME, SessionStore, SocketKind,
    TicketError,
    logging::{Counter, SecurityLog},
    oidc::OidcFailure,
    pages::{self, LoginPage},
    password_login::{PasswordAttempt, PasswordLogin},
    policy::{Authorization, AuthorizedRequest, Policy, constant_equal, cookie},
    rate::{AttemptLimiter, Budget},
    session::random_token,
    valid_challenge,
};
use crate::{
    config::{AuthConfig, AuthMode, ConfigError},
    cors::Access,
};
use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use ipnet::IpNet;
use serde::Deserialize;
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct Service {
    policy: Policy,
    sessions: SessionStore,
    password: Option<PasswordLogin>,
    oidc: Option<super::oidc::Oidc>,
    mode: AuthMode,
    log: Arc<SecurityLog>,
    attempts: Arc<AttemptLimiter>,
}

impl Service {
    pub fn new(config: &AuthConfig, trusted: Vec<IpNet>, sessions: Option<SessionStore>) -> Result<Self, ConfigError> {
        let sessions = sessions.unwrap_or_default();
        let log = Arc::new(SecurityLog::default());
        let attempts = Arc::new(AttemptLimiter::with_log(log.clone()));
        let service = Self {
            policy: Policy::new(&config.public_url, config.mode, trusted, sessions.clone())?,
            password: config
                .mode
                .password()
                .then(|| PasswordLogin::new(config, sessions.clone(), attempts.clone()))
                .transpose()?,
            oidc: config
                .mode
                .oidc()
                .then(|| super::oidc::Oidc::new(config, log.clone()))
                .transpose()?,
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
            self.log.debug("local password hash loaded and validated");
        }
    }
    pub(crate) async fn security_log(&self) {
        let aggregate = async {
            let mut last = [0; Counter::COUNT];
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
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
    pub fn policy(&self) -> &Policy {
        &self.policy
    }
    pub fn sessions(&self) -> &SessionStore {
        &self.sessions
    }

    /// Only bounded auth/ticket bodies belong here; measurement streaming remains
    /// in the transport dispatcher with its original authorization lease.
    pub async fn handle(&self, authorized: &AuthorizedRequest<Bytes>) -> Option<Response<Bytes>> {
        let request = authorized.request();
        let path = request.uri().path();
        if path != "/login" && !path.starts_with("/auth/") && !matches!(path, "/wt/session" | "/ws/session") {
            return None;
        }
        if let Authorization::Preflight(headers) = authorized.authorization() {
            let mut response = response(StatusCode::NO_CONTENT);
            response.headers_mut().extend(headers.clone());
            return Some(response);
        }
        if let Authorization::Authenticated(lease) = authorized.authorization()
            && !lease.is_active()
        {
            return Some(response(StatusCode::FORBIDDEN));
        }
        if request.body().len() > 4096 {
            return Some(response(StatusCode::FORBIDDEN));
        }
        let result = match (request.method(), path) {
            (&Method::GET, "/login") => self.login_page(request).await,
            (&Method::POST, "/auth/oidc/start") => self.oidc_start(authorized).await,
            (&Method::GET, "/auth/oidc/callback") => self.oidc_callback(authorized).await,
            (&Method::POST, "/auth/password") => self.password_login(authorized).await,
            (&Method::GET, "/auth/session") => self.session_info(authorized),
            (&Method::POST, "/auth/logout") => self.logout(authorized),
            (&Method::GET, "/auth/cli") => self.approval_page(authorized, false),
            (&Method::GET, "/auth/browser") => self.approval_page(authorized, true),
            (&Method::POST, "/auth/cli/approve") => self.approve(authorized, false),
            (&Method::POST, "/auth/browser/approve") => self.approve(authorized, true),
            (&Method::POST, "/auth/cli/token") => self.exchange(request, false),
            (&Method::POST, "/auth/browser/token") => self.exchange(request, true),
            (_, "/wt/session" | "/ws/session") => self.ticket(authorized),
            _ => response(StatusCode::NOT_FOUND),
        };
        Some(result)
    }

    async fn login_page(&self, request: &Request<Bytes>) -> Response<Bytes> {
        let provider = self.oidc.as_ref().and_then(|oidc| oidc.ready());
        let Ok(nonce) = random_token::<32>() else {
            return response(StatusCode::SERVICE_UNAVAILABLE);
        };
        let query = query(request);
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
            result
                .headers_mut()
                .extend(pages::security_headers(Some(&provider.origin)).expect("validated provider origin"));
        }
        set_cookie(
            &mut result,
            "__Host-gm_login",
            &nonce,
            SystemTime::now() + Duration::from_secs(600),
            true,
        );
        result
    }

    async fn password_login(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let request = authorized.request();
        let Ok(form) = form(request) else {
            self.log.debug("login rejected reason=form-malformed");
            return login_rejected("failed", "");
        };
        let challenge = value(&form, "challenge");
        let Some(password) = &self.password else {
            return response(StatusCode::NOT_FOUND);
        };
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
                    query_url("/auth/cli", &[("challenge", challenge)])
                } else {
                    "/".into()
                };
                let mut result = redirect(&destination);
                set_cookie(
                    &mut result,
                    "__Host-gm_session",
                    &token,
                    session.session().expires(),
                    true,
                );
                set_cookie(
                    &mut result,
                    "__Host-gm_csrf",
                    session.session().csrf(),
                    session.session().expires(),
                    false,
                );
                clear_cookie(&mut result, "__Host-gm_login");
                let (device, expires) = password.device_cookie(SystemTime::now());
                set_cookie(&mut result, "__Host-gm_device", &device, expires, true);
                result
            }
            Err(failure) => {
                use super::password_login::LoginFailure;
                match failure {
                    LoginFailure::Password => self.log.count(Counter::InvalidPassword),
                    LoginFailure::Throttled => self.log.count(Counter::Throttled),
                    LoginFailure::Capacity => self.log.count(Counter::Capacity),
                    _ => {}
                }
                self.log.debug(match failure {
                    LoginFailure::Failed => "login rejected reason=form-invalid",
                    LoginFailure::Stale => "login rejected reason=form-stale",
                    LoginFailure::Throttled => "login rejected reason=throttled",
                    LoginFailure::Busy => "login rejected reason=verifier-busy",
                    LoginFailure::Capacity => "login rejected reason=session-capacity",
                    LoginFailure::Password => "login rejected reason=password-mismatch",
                });
                login_rejected(failure.notice(), challenge)
            }
        }
    }

    async fn oidc_start(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let Some(oidc) = &self.oidc else {
            return response(StatusCode::NOT_FOUND);
        };
        let request = authorized.request();
        let Ok(form) = form(request) else {
            return self.oidc_rejected(OidcFailure::Failed, "failed", "");
        };
        let challenge = value(&form, "challenge");
        let challenge = if valid_challenge(challenge) { challenge } else { "" };
        let csrf = value(&form, "csrf");
        if text(request, "origin") != self.policy.public_origin()
            || csrf.is_empty()
            || !cookie(request.headers(), "__Host-gm_login").is_some_and(|nonce| constant_equal(nonce, csrf))
        {
            return self.oidc_rejected(OidcFailure::Failed, "stale", challenge);
        }
        let Some(address) = self
            .policy
            .client_address(request.headers(), authorized.connection().peer)
        else {
            return self.oidc_rejected(OidcFailure::Failed, "throttled", challenge);
        };
        let prior = cookie(request.headers(), "__Host-gm_session").and_then(|token| self.sessions.lookup(token));
        match oidc.start(address, challenge.to_owned(), prior).await {
            Ok(started) => {
                let mut result = redirect(&started.url);
                result.headers_mut().extend(
                    pages::security_headers(Some(&started.provider.origin)).expect("validated provider origin"),
                );
                result.headers_mut().append(
                    header::SET_COOKIE,
                    HeaderValue::from_str(&format!(
                        "__Host-gm_oidc={}; Path=/; Max-Age=600; Secure; HttpOnly; SameSite=Lax",
                        started.browser
                    ))
                    .expect("transaction cookie"),
                );
                result
            }
            Err(failure) => self.oidc_rejected(failure, "provider", challenge),
        }
    }

    async fn oidc_callback(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let Some(oidc) = &self.oidc else {
            return response(StatusCode::NOT_FOUND);
        };
        let request = authorized.request();
        let fields = query(request);
        let unique = |key: &str| {
            let mut values = fields.iter().filter(|(name, _)| name == key);
            let value = values.next().map(|(_, value)| value.as_str());
            if values.next().is_some() { None } else { value }
        };
        let mut result = async {
            let state = unique("state")
                .filter(|value| value.len() == 43)
                .ok_or(OidcFailure::Failed)?;
            let code = unique("code")
                .filter(|value| !value.is_empty() && value.len() <= 2048 && !value.chars().any(char::is_control))
                .ok_or(OidcFailure::Failed)?;
            if fields.iter().any(|(key, _)| key == "error") || fields.iter().filter(|(key, _)| key == "iss").count() > 1
            {
                return Err(OidcFailure::Failed);
            }
            let browser = cookie(request.headers(), "__Host-gm_oidc").ok_or(OidcFailure::Failed)?;
            let address = self
                .policy
                .client_address(request.headers(), authorized.connection().peer)
                .ok_or(OidcFailure::Failed)?;
            if !self.attempts.allow(Budget::OidcExchange, address) {
                return Err(OidcFailure::Throttled);
            }
            let identity = oidc.finish(state, browser, code, unique("iss")).await?;
            let (token, session) = self
                .sessions
                .create(&identity.subject, &identity.name, oidc.name(), None)
                .map_err(|failure| match failure {
                    super::SessionError::Capacity => OidcFailure::Capacity,
                    super::SessionError::RandomUnavailable => OidcFailure::Failed,
                })?;
            if let Some(prior) = identity.prior {
                self.sessions.revoke(&prior);
            }
            let mut result = html(StatusCode::OK, pages::continue_page(&identity.challenge, false));
            set_cookie(
                &mut result,
                "__Host-gm_session",
                &token,
                session.session().expires(),
                true,
            );
            set_cookie(
                &mut result,
                "__Host-gm_csrf",
                session.session().csrf(),
                session.session().expires(),
                false,
            );
            clear_cookie(&mut result, "__Host-gm_login");
            self.log.count(Counter::Oidc);
            Ok::<_, OidcFailure>(result)
        }
        .await
        .unwrap_or_else(|failure| self.oidc_rejected(failure, "failed", ""));
        clear_cookie(&mut result, "__Host-gm_oidc");
        result
    }

    fn oidc_rejected(&self, failure: OidcFailure, notice: &str, challenge: &str) -> Response<Bytes> {
        self.log.count(Counter::OidcFailure);
        match failure {
            OidcFailure::ReplayExpiry => self.log.count(Counter::ReplayExpiry),
            OidcFailure::GroupDenial => self.log.count(Counter::GroupDenial),
            OidcFailure::Capacity => self.log.count(Counter::Capacity),
            OidcFailure::Throttled => self.log.count(Counter::Throttled),
            OidcFailure::Failed => {}
        }
        self.log.debug(match failure {
            OidcFailure::ReplayExpiry => "login rejected reason=transaction-replay",
            OidcFailure::GroupDenial => "login rejected reason=group-denial",
            OidcFailure::Capacity => "login rejected reason=capacity",
            OidcFailure::Throttled => "login rejected reason=throttled",
            OidcFailure::Failed => "login rejected reason=oidc-failure",
        });
        login_rejected(notice, challenge)
    }

    fn session_info(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let Some(lease) = principal(authorized) else {
            return response(StatusCode::FORBIDDEN);
        };
        let session = lease.session();
        json_response(
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
            let mut state = self.sessions.0.lock().expect("session mutex poisoned");
            if !state.contains(&lease.session) || !lease.is_active() {
                return response(StatusCode::FORBIDDEN);
            }
            let keys: Vec<_> = if value(&form, "scope") == "all" {
                state
                    .sessions
                    .values()
                    .filter(|session| session.subject() == lease.session().subject())
                    .map(|session| session.hash)
                    .collect()
            } else {
                vec![lease.session.0.hash]
            };
            for key in keys {
                state.remove(&key);
            }
        }
        self.log.count(Counter::Logout);
        let mut result = redirect("/login?reason=signed_out");
        for name in ["__Host-gm_session", "__Host-gm_login", "__Host-gm_csrf"] {
            clear_cookie(&mut result, name);
        }
        result
    }

    fn approval_page(&self, authorized: &AuthorizedRequest<Bytes>, browser: bool) -> Response<Bytes> {
        let request = authorized.request();
        let query = query(request);
        let challenge = value(&query, "challenge");
        if !valid_challenge(challenge) {
            return response(StatusCode::FORBIDDEN);
        }
        let Some(client) = self
            .policy
            .client_address(request.headers(), authorized.connection().peer)
        else {
            return response(StatusCode::FORBIDDEN);
        };
        if !browser && let Some(destination) = self.sessions.browser_approval_redirect(challenge) {
            return redirect(&destination);
        }
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
            if !super::secure_browser_origin(origin) {
                return response(StatusCode::FORBIDDEN);
            }
            if self.sessions.browser_approval_redirect(challenge).is_none()
                && !self.attempts.allow(Budget::BrowserApproval, client)
            {
                return response(StatusCode::FORBIDDEN);
            }
            self.sessions
                .begin_browser_approval(challenge, origin, session.as_ref(), client)
        } else {
            let Some(session) = &session else {
                return redirect(&query_url("/login", &[("challenge", challenge)]));
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
                    return redirect(&query_url("/login", &[("challenge", challenge)]));
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
        let payload = serde_json::from_slice::<Payload>(request.body());
        let exchange = match payload {
            Ok(payload) => {
                if browser {
                    self.sessions.exchange_browser(&payload.verifier, origin)
                } else {
                    self.sessions.exchange_cli(&payload.verifier)
                }
            }
            Err(_) if browser => Err(ExchangeError::InvalidVerifier),
            Err(_) => Ok(Exchange::Pending),
        };
        let mut result = match exchange {
            Ok(Exchange::Pending) => json_response(StatusCode::ACCEPTED, json!({"status":"pending"})),
            Ok(Exchange::Issued { token, lease }) => {
                let expires = lease.session().expires();
                if browser {
                    json_response(
                        StatusCode::OK,
                        json!({"token":token, "expires":unix_ms(expires), "remainingMs":remaining_ms(expires), "maximumLifetimeMs":SESSION_LIFETIME.as_millis() as u64}),
                    )
                } else {
                    json_response(StatusCode::OK, json!({"token":token, "expires":rfc3339(expires)}))
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

    fn ticket(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let request = authorized.request();
        if request.method() != Method::POST {
            let mut response = response(StatusCode::METHOD_NOT_ALLOWED);
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("POST"));
            return response;
        }
        let Some(lease) = principal(authorized) else {
            return response(StatusCode::FORBIDDEN);
        };
        let query = query(request);
        let kind = if request.uri().path() == "/ws/session" {
            SocketKind::WebSocket
        } else {
            SocketKind::WebTransport
        };
        match self.sessions.mint_ticket(
            lease,
            self.policy.public_origin(),
            value(&query, "target"),
            text(request, "origin"),
            kind,
        ) {
            Ok(ticket) => json_response(
                StatusCode::OK,
                json!({"token":ticket.token, "expires":unix_ms(ticket.expires)}),
            ),
            Err(TicketError::InvalidTarget) => error_response(StatusCode::BAD_REQUEST, "invalid socket target\n"),
            Err(TicketError::NoSession) => error_response(StatusCode::FORBIDDEN, "no session to bind a token to\n"),
            Err(TicketError::Capacity) => {
                let mut result = error_response(StatusCode::TOO_MANY_REQUESTS, "webtransport token capacity reached\n");
                result
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                result
            }
            Err(TicketError::RandomUnavailable) => response(StatusCode::SERVICE_UNAVAILABLE),
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
    let mut response = Response::new(Bytes::new());
    *response.status_mut() = status;
    *response.headers_mut() = pages::security_headers(None).expect("static auth CSP");
    response.headers_mut().insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000"),
    );
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
fn json_response(status: StatusCode, value: serde_json::Value) -> Response<Bytes> {
    let mut response = response(status);
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    *response.body_mut() = value.to_string().into();
    response
}
fn error_response(status: StatusCode, body: &'static str) -> Response<Bytes> {
    let mut response = response(status);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    *response.body_mut() = Bytes::from_static(body.as_bytes());
    response
}
fn redirect(destination: &str) -> Response<Bytes> {
    let mut response = response(StatusCode::SEE_OTHER);
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(destination).expect("encoded redirect"),
    );
    response
}
fn login_rejected(notice: &str, challenge: &str) -> Response<Bytes> {
    let mut fields = vec![("error", notice)];
    if valid_challenge(challenge) {
        fields.push(("challenge", challenge));
    }
    redirect(&query_url("/login", &fields))
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
fn query(request: &Request<Bytes>) -> Vec<(String, String)> {
    form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .into_owned()
        .collect()
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
    if !text(request, "content-type")
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
    let values = parse_form_pairs(body)?;
    let mut names = std::collections::BTreeSet::new();
    if values.iter().any(|(name, _)| !names.insert(name)) {
        return Err(());
    }
    Ok(values)
}
fn parse_form_pairs(raw: &str) -> Result<Vec<(String, String)>, ()> {
    raw.split('&')
        .filter(|field| !field.is_empty())
        .map(|field| {
            if field.contains(';') {
                return Err(());
            }
            let (key, value) = field.split_once('=').unwrap_or((field, ""));
            Ok((decode_form(key)?, decode_form(value)?))
        })
        .collect()
}
fn decode_form(raw: &str) -> Result<String, ()> {
    let mut out = Vec::with_capacity(raw.len());
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        out.push(match byte {
            b'+' => b' ',
            b'%' => {
                let high = (bytes.next().ok_or(())? as char).to_digit(16).ok_or(())?;
                let low = (bytes.next().ok_or(())? as char).to_digit(16).ok_or(())?;
                (high * 16 + low) as u8
            }
            byte => byte,
        });
    }
    String::from_utf8(out).map_err(|_| ())
}
fn set_cookie(response: &mut Response<Bytes>, name: &str, value: &str, expires: SystemTime, http_only: bool) {
    let age = expires.duration_since(SystemTime::now()).unwrap_or_default().as_secs();
    let value = format!(
        "{name}={value}; Path=/; Expires={}; Max-Age={age}; Secure; SameSite=Strict{}",
        httpdate::fmt_http_date(expires),
        if http_only { "; HttpOnly" } else { "" }
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&value).expect("generated cookie"),
    );
}
fn clear_cookie(response: &mut Response<Bytes>, name: &str) {
    let value = format!(
        "{name}=; Path=/; Expires={}; Max-Age=0; Secure; HttpOnly; SameSite=Strict",
        httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(1))
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&value).expect("generated cookie"),
    );
}
fn rfc3339(time: SystemTime) -> String {
    let [year, month, day, hour, minute, second] = crate::log::utc(time);
    let mut text = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    let nanos = time.duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    if nanos != 0 {
        text.push('.');
        text.push_str(format!("{nanos:09}").trim_end_matches('0'));
    }
    text.push('Z');
    text
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

    #[test]
    fn auth_forms_require_unambiguous_body_fields() {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/auth/password?password=url-secret&csrf=url-proof")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Bytes::from_static(b"challenge=example"))
            .unwrap();
        let fields = form(&request).unwrap();
        assert_eq!(value(&fields, "password"), "");
        assert_eq!(value(&fields, "csrf"), "");
        let duplicate = request.map(|_| Bytes::from_static(b"password=first&password=second"));
        assert!(form(&duplicate).is_err());
        let wrong_type = Request::builder()
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Bytes::from_static(b"password=secret"))
            .unwrap();
        assert!(form(&wrong_type).is_err());
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
        service.handle(&request).await.expect("controller endpoint")
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
        }, vec![], None).unwrap());
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
            None,
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
            None,
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
            let service = Service::new(&config, vec!["192.0.2.1/32".parse().unwrap()], None).unwrap();
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
            None,
        )
        .unwrap();
        let login = call(&service, Method::GET, "/login", &[], String::new()).await;
        assert_eq!(login.status(), StatusCode::OK);
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
}

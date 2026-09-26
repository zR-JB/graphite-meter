//! HTTP authentication controller. Requests arrive only after policy authorization.
use super::{
    ApprovalError, ApprovalKind, AuthLease, Exchange, ExchangeError, SESSION_LIFETIME,
    SessionStore, SocketKind, TicketError,
    pages::{self, ApprovalPage, ContinuePage, DonePage, LoginPage},
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
use askama::Template;
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
    attempts: Arc<AttemptLimiter>,
}

impl Service {
    pub fn new(
        config: &AuthConfig,
        trusted: Vec<IpNet>,
        sessions: Option<SessionStore>,
    ) -> Result<Self, ConfigError> {
        let sessions = sessions.unwrap_or_default();
        let attempts = Arc::new(AttemptLimiter::new());
        Ok(Self {
            policy: Policy::new(&config.public_url, config.mode, trusted, sessions.clone())?,
            password: config
                .mode
                .password()
                .then(|| PasswordLogin::new(config, sessions.clone(), attempts.clone()))
                .transpose()?,
            oidc: config
                .mode
                .oidc()
                .then(|| super::oidc::Oidc::new(config))
                .transpose()?,
            mode: config.mode,
            sessions,
            attempts,
        })
    }
    pub async fn initialize(&self) -> Result<(), ConfigError> {
        if let Some(oidc) = &self.oidc {
            let result = oidc.provider().await;
            if self.mode == AuthMode::Oidc {
                result?;
            }
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
        if path != "/login"
            && !path.starts_with("/auth/")
            && !matches!(path, "/wt/session" | "/ws/session")
        {
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
        let provider = if let Some(oidc) = &self.oidc {
            oidc.provider().await.ok()
        } else {
            None
        };
        let Ok(nonce) = random_token::<32>() else {
            return response(StatusCode::SERVICE_UNAVAILABLE);
        };
        let query = query(request);
        let challenge = value(&query, "challenge");
        let challenge = if valid_challenge(challenge) {
            challenge
        } else {
            ""
        };
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
            &LoginPage {
                csrf: &nonce,
                provider: self.oidc.as_ref().map_or("", |oidc| oidc.name()),
                challenge,
                password: self.password.is_some(),
                oidc: self.oidc.is_some(),
                oidc_ready: provider.is_some(),
                notice,
                status,
            },
        );
        if let Some(provider) = provider {
            result.headers_mut().extend(
                pages::security_headers(Some(&provider.origin)).expect("validated provider origin"),
            );
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
                csrf: value(&form, "csrf"),
                password: value(&form, "password"),
                prior_session: cookie(request.headers(), "__Host-gm_session"),
            })
            .await;
        match result {
            Ok((token, session)) => {
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
                result
            }
            Err(failure) => login_rejected(failure.notice(), challenge),
        }
    }

    async fn oidc_start(&self, authorized: &AuthorizedRequest<Bytes>) -> Response<Bytes> {
        let Some(oidc) = &self.oidc else {
            return response(StatusCode::NOT_FOUND);
        };
        let request = authorized.request();
        let Ok(form) = form(request) else {
            return login_rejected("failed", "");
        };
        let challenge = value(&form, "challenge");
        let challenge = if valid_challenge(challenge) {
            challenge
        } else {
            ""
        };
        let csrf = value(&form, "csrf");
        if text(request, "origin") != self.policy.public_origin()
            || csrf.is_empty()
            || !cookie(request.headers(), "__Host-gm_login")
                .is_some_and(|nonce| constant_equal(nonce, csrf))
        {
            return login_rejected("stale", challenge);
        }
        let Some(address) = self
            .policy
            .client_address(request.headers(), authorized.connection().peer)
        else {
            return login_rejected("throttled", challenge);
        };
        let prior = cookie(request.headers(), "__Host-gm_session")
            .and_then(|token| self.sessions.lookup(token));
        match oidc.start(address, challenge.to_owned(), prior).await {
            Ok(started) => {
                let mut result = redirect(&started.url);
                if let Ok(provider) = oidc.provider().await {
                    *result.headers_mut() = pages::security_headers(Some(&provider.origin))
                        .expect("validated provider origin");
                    result.headers_mut().insert(
                        header::LOCATION,
                        HeaderValue::from_str(&started.url).expect("authorization URL"),
                    );
                }
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
            Err(_) => login_rejected("provider", challenge),
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
                .ok_or(())?;
            let code = unique("code")
                .filter(|value| {
                    !value.is_empty() && value.len() <= 2048 && !value.chars().any(char::is_control)
                })
                .ok_or(())?;
            if fields.iter().any(|(key, _)| key == "error")
                || fields.iter().filter(|(key, _)| key == "iss").count() > 1
            {
                return Err(());
            }
            let browser = cookie(request.headers(), "__Host-gm_oidc").ok_or(())?;
            let address = self
                .policy
                .client_address(request.headers(), authorized.connection().peer)
                .ok_or(())?;
            if !self.attempts.allow(Budget::OidcExchange, address) {
                return Err(());
            }
            let identity = oidc
                .finish(state, browser, code, unique("iss"))
                .await
                .map_err(|_| ())?;
            let (token, session) = self
                .sessions
                .create(&identity.subject, &identity.name, oidc.name(), None)
                .map_err(|_| ())?;
            if let Some(prior) = identity.prior {
                self.sessions.revoke(&prior);
            }
            let mut result = html(
                StatusCode::OK,
                &ContinuePage {
                    challenge: &identity.challenge,
                    opening: false,
                },
            );
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
            Ok::<_, ()>(result)
        }
        .await
        .unwrap_or_else(|()| login_rejected("failed", ""));
        clear_cookie(&mut result, "__Host-gm_oidc");
        result
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
        let mut result = redirect("/login?reason=signed_out");
        for name in ["__Host-gm_session", "__Host-gm_login", "__Host-gm_csrf"] {
            clear_cookie(&mut result, name);
        }
        result
    }

    fn approval_page(
        &self,
        authorized: &AuthorizedRequest<Bytes>,
        browser: bool,
    ) -> Response<Bytes> {
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
            cookie(request.headers(), "__Host-gm_session")
                .and_then(|token| self.sessions.lookup(token))
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
                        return html(
                            StatusCode::OK,
                            &ContinuePage {
                                challenge,
                                opening: true,
                            },
                        );
                    }
                    return redirect(&query_url("/login", &[("challenge", challenge)]));
                };
                html(
                    StatusCode::OK,
                    &ApprovalPage {
                        browser_capacity: false,
                        client_limit: 8,
                        browser_origin: view.browser_origin.as_deref().unwrap_or(""),
                        code: &view.code,
                        csrf: session.session().csrf(),
                        challenge,
                    },
                )
            }
            Err(ApprovalError::GrantCapacity) => capacity_page(origin),
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
            Ok(()) => html(StatusCode::OK, &DonePage { browser }),
            Err(ApprovalError::GrantCapacity) => {
                let origin = self
                    .sessions
                    .browser_approval_redirect(challenge)
                    .and_then(|path| path.split_once('?').map(|(_, query)| query.to_owned()))
                    .map(|query| {
                        form_urlencoded::parse(query.as_bytes())
                            .find(|(key, _)| key == "client_origin")
                            .map(|(_, value)| value.into_owned())
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                capacity_page(&origin)
            }
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
            Ok(Exchange::Pending) => {
                json_response(StatusCode::ACCEPTED, json!({"status":"pending"}))
            }
            Ok(Exchange::Issued { token, lease }) => {
                let expires = lease.session().expires();
                if browser {
                    json_response(
                        StatusCode::OK,
                        json!({"token":token, "expires":unix_ms(expires), "remainingMs":remaining_ms(expires), "maximumLifetimeMs":SESSION_LIFETIME.as_millis() as u64}),
                    )
                } else {
                    json_response(
                        StatusCode::OK,
                        json!({"token":token, "expires":rfc3339(expires)}),
                    )
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
            return response(StatusCode::METHOD_NOT_ALLOWED);
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
            Err(TicketError::InvalidTarget) => {
                error_response(StatusCode::BAD_REQUEST, "invalid socket target\n")
            }
            Err(TicketError::NoSession) => {
                error_response(StatusCode::FORBIDDEN, "no session to bind a token to\n")
            }
            Err(TicketError::Capacity) => {
                let mut result = error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    "webtransport token capacity reached\n",
                );
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
    response
}
fn html(status: StatusCode, template: &impl Template) -> Response<Bytes> {
    match template.render() {
        Ok(body) => {
            let mut response = response(status);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            *response.body_mut() = body.into();
            response
        }
        Err(_) => response(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
fn json_response(status: StatusCode, value: serde_json::Value) -> Response<Bytes> {
    let mut response = response(status);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
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
fn capacity_page(origin: &str) -> Response<Bytes> {
    html(
        StatusCode::TOO_MANY_REQUESTS,
        &ApprovalPage {
            browser_capacity: true,
            client_limit: 8,
            browser_origin: origin,
            code: "",
            csrf: "",
            challenge: "",
        },
    )
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
fn set_cookie(
    response: &mut Response<Bytes>,
    name: &str,
    value: &str,
    expires: SystemTime,
    http_only: bool,
) {
    let age = expires
        .duration_since(SystemTime::now())
        .unwrap_or_default()
        .as_secs();
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
    let elapsed = time
        .duration_since(UNIX_EPOCH)
        .expect("session expiry after epoch");
    let (days, seconds) = (elapsed.as_secs() / 86_400, elapsed.as_secs() % 86_400);
    let era_day = days + 719_468;
    let (era, day_of_era) = (era_day / 146_097, era_day % 146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    let mut text = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
    if elapsed.subsec_nanos() != 0 {
        text.push('.');
        text.push_str(format!("{:09}", elapsed.subsec_nanos()).trim_end_matches('0'));
    }
    text.push('Z');
    text
}
fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
    use sha2::{Digest, Sha256};

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
    async fn password_browser_cli_ticket_and_logout_flow_through_policy_and_controller() {
        const PUBLIC: &str = "https://meter.example";
        const REMOTE: &str = "https://client.example";
        let service = Service::new(&AuthConfig { mode: AuthMode::Password, public_url: PUBLIC.into(), password_hash: "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0".into(), ..AuthConfig::default() }, vec![], None).unwrap();
        let login = call(&service, Method::GET, "/login", &[], String::new()).await;
        assert_eq!(login.status(), StatusCode::OK);
        let nonce = set_cookie_value(&login, "__Host-gm_login");
        let nonce_cookie = format!("__Host-gm_login={nonce}");
        let signed_in = call(
            &service,
            Method::POST,
            "/auth/password",
            &[
                ("cookie", &nonce_cookie),
                ("origin", PUBLIC),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            encoded(&[
                ("csrf", &nonce),
                ("password", "correct horse battery staple"),
            ]),
        )
        .await;
        assert_eq!(signed_in.status(), StatusCode::SEE_OTHER);
        let raw_session = set_cookie_value(&signed_in, "__Host-gm_session");
        let session_cookie = format!("__Host-gm_session={raw_session}");
        let csrf = set_cookie_value(&signed_in, "__Host-gm_csrf");
        let info = call(
            &service,
            Method::GET,
            "/auth/session",
            &[("cookie", &session_cookie)],
            String::new(),
        )
        .await;
        let info: serde_json::Value = serde_json::from_slice(info.body()).unwrap();
        assert_eq!(info["provider"], "local");
        assert_eq!(info["csrf"], csrf);
        assert!(info["expires"].is_string());
        assert_eq!(info["maximumLifetimeMs"], 28_800_000);

        let verifier = "v".repeat(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let page_url = query_url(
            "/auth/browser",
            &[("challenge", &challenge), ("client_origin", REMOTE)],
        );
        let opening = call(
            &service,
            Method::GET,
            &page_url,
            &[
                ("sec-fetch-site", "cross-site"),
                ("sec-fetch-mode", "navigate"),
                ("sec-fetch-dest", "document"),
            ],
            String::new(),
        )
        .await;
        assert_eq!(opening.status(), StatusCode::OK);
        let approval = call(
            &service,
            Method::GET,
            &page_url,
            &[("cookie", &session_cookie)],
            String::new(),
        )
        .await;
        assert_eq!(approval.status(), StatusCode::OK);
        let approved = call(
            &service,
            Method::POST,
            "/auth/browser/approve",
            &[
                ("cookie", &session_cookie),
                ("origin", PUBLIC),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            encoded(&[("csrf", &csrf), ("challenge", &challenge)]),
        )
        .await;
        assert_eq!(approved.status(), StatusCode::OK);
        let token = call(
            &service,
            Method::POST,
            "/auth/browser/token",
            &[("origin", REMOTE), ("content-type", "application/json")],
            json!({"verifier":verifier}).to_string(),
        )
        .await;
        assert_eq!(token.status(), StatusCode::OK);
        assert_eq!(token.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], REMOTE);
        assert!(
            !token
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
        );
        let token: serde_json::Value = serde_json::from_slice(token.body()).unwrap();
        assert!(token["expires"].is_u64());
        let bearer = format!("Bearer {}", token["token"].as_str().unwrap());
        let ticket = call(
            &service,
            Method::POST,
            &query_url(
                "/wt/session",
                &[("target", "https://meter.example:8443/wt/ping")],
            ),
            &[("origin", REMOTE), ("authorization", &bearer)],
            String::new(),
        )
        .await;
        assert_eq!(ticket.status(), StatusCode::OK);
        let ticket: serde_json::Value = serde_json::from_slice(ticket.body()).unwrap();
        let connect = Request::builder()
            .method(Method::CONNECT)
            .uri(query_url(
                "/wt/ping",
                &[("token", ticket["token"].as_str().unwrap())],
            ))
            .header(header::HOST, "meter.example:8443")
            .header(header::ORIGIN, REMOTE)
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

        let cli_verifier = "native-client";
        let cli_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(cli_verifier.as_bytes()));
        assert_eq!(
            call(
                &service,
                Method::GET,
                &query_url("/auth/cli", &[("challenge", &cli_challenge)]),
                &[("cookie", &session_cookie)],
                String::new()
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            call(
                &service,
                Method::POST,
                "/auth/cli/approve",
                &[
                    ("cookie", &session_cookie),
                    ("origin", PUBLIC),
                    ("content-type", "application/x-www-form-urlencoded")
                ],
                encoded(&[("csrf", &csrf), ("challenge", &cli_challenge)])
            )
            .await
            .status(),
            StatusCode::OK
        );
        let cli = call(
            &service,
            Method::POST,
            "/auth/cli/token",
            &[("content-type", "application/json")],
            json!({"verifier":cli_verifier}).to_string(),
        )
        .await;
        let cli: serde_json::Value = serde_json::from_slice(cli.body()).unwrap();
        assert!(cli["expires"].is_string());
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
        assert!(
            service
                .sessions()
                .lookup_bearer(cli["token"].as_str().unwrap())
                .is_none()
        );
        assert!(
            service
                .sessions()
                .lookup_bearer(token["token"].as_str().unwrap())
                .is_none()
        );
    }
}

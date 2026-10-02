use graphite_meter_core::route::Kind;
use graphite_meter_server::{
    auth::{
        ApprovalKind, AuthLease, Exchange, SessionLease, SessionStore,
        policy::{Authorization, Connection, Listener, Policy, Refusal},
        valid_challenge,
    },
    config::AuthMode,
};
use http::{Request, header};
use std::net::SocketAddr;

const PUBLIC: &str = "https://meter.example";
const CLIENT: &str = "https://client.example";

fn policy(store: &SessionStore) -> Policy {
    Policy::new(
        PUBLIC,
        AuthMode::Password,
        vec!["10.0.0.0/8".parse().unwrap()],
        store.clone(),
    )
    .unwrap()
}

/// A grant's bearer token, issued by an approved exchange for the browser `origin`, or a terminal.
fn grant(store: &SessionStore, session: &SessionLease, origin: Option<&str>) -> String {
    let verifier = "v".repeat(43);
    let challenge = graphite_meter_core::approval::challenge(&verifier);
    let client = "192.0.2.1".parse().unwrap();
    let kind = match origin {
        Some(origin) => store
            .begin_browser_approval(&valid_challenge(&challenge).unwrap(), origin, Some(session), client)
            .map(|_| ApprovalKind::Browser),
        None => store
            .begin_cli_approval(session, &valid_challenge(&challenge).unwrap(), client)
            .map(|_| ApprovalKind::Cli),
    };
    store.approve(session, &challenge, kind.unwrap()).unwrap();
    let exchange = match origin {
        Some(origin) => store.exchange_browser(&verifier, origin),
        None => store.exchange_cli(&verifier),
    };
    let Ok(Exchange::Issued { token, .. }) = exchange else {
        panic!("the approved exchange issued no grant")
    };
    token
}

fn peer() -> SocketAddr {
    "192.0.2.1:4000".parse().unwrap()
}

fn request(method: &str, path: &str) -> Request<()> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "meter.example")
        .body(())
        .unwrap()
}

fn set(request: &mut Request<()>, name: impl http::header::IntoHeaderName, value: &str) {
    request.headers_mut().insert(name, value.parse().unwrap());
}

fn cookie(request: &mut Request<()>, token: &str) {
    request
        .headers_mut()
        .insert(header::COOKIE, format!("__Host-gm_session={token}").parse().unwrap());
}

fn bearer(request: &mut Request<()>, token: &str) {
    request
        .headers_mut()
        .insert(header::AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
}

fn allowed(policy: &Policy, request: &Request<()>) -> AuthLease {
    match evaluate(
        policy,
        request,
        peer(),
        true,
        Listener {
            ui: true,
            webtransport: false,
        },
    ) {
        Ok(Authorization::Authenticated(lease)) => lease,
        _ => panic!("request should authenticate"),
    }
}

fn refused(policy: &Policy, request: &Request<()>, refusal: Refusal) {
    assert!(
        matches!(evaluate(policy, request, peer(), true, Listener { ui: true, webtransport: false }), Err(actual) if actual == refusal)
    );
}

#[test]
fn tls_hostnames_and_proxy_evidence_have_distinct_trust_boundaries() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let mut req = request("GET", "/login");
    let trusted: SocketAddr = "10.1.2.3:4000".parse().unwrap();
    assert!(policy.trust(&req, peer(), true).canonical);
    set(&mut req, header::HOST, "meter.example:8443");
    let trust = policy.trust(&req, peer(), true);
    assert!(trust.secure && !trust.canonical);
    set(&mut req, header::HOST, "other.example");
    assert!(!policy.trust(&req, peer(), true).secure);
    set(&mut req, header::HOST, "meter.example");
    req.headers_mut().append(header::HOST, "other.example".parse().unwrap());
    assert!(!policy.trust(&req, peer(), true).secure);
    req.headers_mut().remove(header::HOST);
    set(&mut req, header::HOST, "other.example");
    set(&mut req, "x-forwarded-proto", "https");
    set(&mut req, "x-forwarded-host", "meter.example");
    assert!(!policy.trust(&req, peer(), false).secure);
    assert!(policy.trust(&req, trusted, false).canonical);
    req.headers_mut().append("x-forwarded-proto", "https".parse().unwrap());
    assert!(!policy.trust(&req, trusted, false).secure);
    set(&mut req, "x-forwarded-proto", "https,http");
    assert!(!policy.trust(&req, trusted, false).secure);
    set(&mut req, "x-real-ip", "::ffff:198.51.100.4");
    assert_eq!(
        policy.client_address(req.headers(), trusted).unwrap().to_string(),
        "198.51.100.4"
    );
    set(&mut req, "x-forwarded-for", "198.51.100.4");
    assert!(policy.client_address(req.headers(), trusted).is_none());
    assert_eq!(policy.client_address(req.headers(), peer()), Some(peer().ip()));
}

#[test]
fn cookie_measurements_require_positive_origin_evidence_and_mutation_csrf() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let (token, session) = store.create("operator", "Operator", "local", None).unwrap();
    let mut req = request("GET", "/download");
    cookie(&mut req, &token);
    refused(&policy, &req, Refusal::Forbidden);
    set(&mut req, "sec-fetch-site", "same-origin");
    assert!(!allowed(&policy, &req).is_bearer());
    set(&mut req, "sec-fetch-site", "same-site");
    refused(&policy, &req, Refusal::Forbidden);
    set(&mut req, header::ORIGIN, PUBLIC);
    allowed(&policy, &req);
    *req.method_mut() = http::Method::POST;
    *req.uri_mut() = "/upload".parse().unwrap();
    refused(&policy, &req, Refusal::Forbidden);
    set(&mut req, "x-csrf-token", session.session().csrf());
    allowed(&policy, &req);
    set(&mut req, header::ORIGIN, CLIENT);
    refused(&policy, &req, Refusal::Forbidden);

    *req.method_mut() = http::Method::GET;
    *req.uri_mut() = "/ws/ping".parse().unwrap();
    req.headers_mut().remove(header::ORIGIN);
    set(&mut req, "sec-fetch-site", "same-origin");
    refused(&policy, &req, Refusal::Forbidden);
    set(&mut req, header::ORIGIN, PUBLIC);
    allowed(&policy, &req);
}

#[test]
fn explicit_credentials_never_fall_back_to_ambient_cookies() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let (token, session) = store.create("operator", "Operator", "local", None).unwrap();
    let cli = grant(&store, &session, None);
    let mut req = request("GET", "/download");
    cookie(&mut req, &token);
    set(&mut req, header::ORIGIN, PUBLIC);
    bearer(&mut req, "invalid");
    refused(&policy, &req, Refusal::AuthenticationRequired);
    // As in Go, a repeated Authorization is forbidden before any credential is read.
    set(&mut req, header::AUTHORIZATION, "");
    req.headers_mut()
        .append(header::AUTHORIZATION, "Bearer invalid".parse().unwrap());
    refused(&policy, &req, Refusal::Ambiguous);
    bearer(&mut req, &cli);
    req.headers_mut()
        .append(header::AUTHORIZATION, "Bearer invalid".parse().unwrap());
    refused(&policy, &req, Refusal::Ambiguous);
    set(&mut req, header::AUTHORIZATION, "");
    refused(&policy, &req, Refusal::AuthenticationRequired);
    bearer(&mut req, &cli);
    assert_eq!(allowed(&policy, &req).provider(), "cli");
    *req.uri_mut() = "/ws/ping?token=".parse().unwrap();
    refused(&policy, &req, Refusal::AuthenticationRequired);
    *req.uri_mut() = "/auth/session".parse().unwrap();
    refused(&policy, &req, Refusal::Forbidden);
    req.headers_mut().remove(header::AUTHORIZATION);
    assert_eq!(allowed(&policy, &req).provider(), "local");
    store.revoke(&session);
    refused(&policy, &req, Refusal::AuthenticationRequired);
}

#[test]
fn ambiguous_cookie_and_origin_evidence_cannot_authorize_a_measurement() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let (token, session) = store.create("operator", "Operator", "local", None).unwrap();
    let mut req = request("POST", "/upload");
    cookie(&mut req, &token);
    set(&mut req, header::ORIGIN, PUBLIC);
    set(&mut req, "sec-fetch-site", "same-origin");
    set(&mut req, "x-csrf-token", session.session().csrf());
    allowed(&policy, &req);
    for other in ["theme=é", "__Host-gm_session=ab\"c"] {
        req.headers_mut().append(header::COOKIE, other.parse().unwrap());
        allowed(&policy, &req);
    }

    for duplicate in ["__Host-gm_session=other", "__Host-gm_session"] {
        req.headers_mut().append(header::COOKIE, duplicate.parse().unwrap());
        refused(&policy, &req, Refusal::AuthenticationRequired);
        req.headers_mut().remove(header::COOKIE);
        cookie(&mut req, &token);
    }

    for (name, value) in [
        (header::ORIGIN, PUBLIC),
        (header::HeaderName::from_static("sec-fetch-site"), "same-origin"),
        (
            header::HeaderName::from_static("x-csrf-token"),
            session.session().csrf(),
        ),
    ] {
        req.headers_mut().append(name.clone(), value.parse().unwrap());
        refused(&policy, &req, Refusal::Ambiguous);
        req.headers_mut().remove(name.clone());
        req.headers_mut().insert(name, value.parse().unwrap());
    }
}

#[test]
fn browser_grants_are_audience_and_route_scoped() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let (_, session) = store.create("operator", "Operator", "local", None).unwrap();
    let token = grant(&store, &session, Some(CLIENT));
    let mut req = request("POST", "/upload");
    bearer(&mut req, &token);
    refused(&policy, &req, Refusal::Forbidden);
    set(&mut req, header::ORIGIN, CLIENT);
    assert_eq!(allowed(&policy, &req).browser_origin(), Some(CLIENT));
    for path in ["/servers", "/auth/session", "/", "/unknown"] {
        *req.uri_mut() = path.parse().unwrap();
        refused(&policy, &req, Refusal::Forbidden);
    }
    *req.uri_mut() = "/upload".parse().unwrap();
    set(&mut req, header::ORIGIN, PUBLIC);
    refused(&policy, &req, Refusal::Forbidden);
}

#[test]
fn webtransport_uses_no_cookie_and_burns_tickets_even_with_bearer() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let (token, session) = store.create("operator", "Operator", "local", None).unwrap();
    let lease = AuthLease::cookie(session.clone());
    let cli = grant(&store, &session, None);
    let target = "https://meter.example/wt/ping";
    let listener = Listener {
        ui: false,
        webtransport: true,
    };
    let mut req = request("CONNECT", "/wt/ping");
    cookie(&mut req, &token);
    set(&mut req, header::ORIGIN, PUBLIC);
    set(&mut req, "x-csrf-token", session.session().csrf());
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, listener),
        Err(Refusal::AuthenticationRequired)
    ));
    let ticket = store
        .mint_ticket(&lease, PUBLIC, target, PUBLIC, Kind::WebTransport)
        .unwrap();
    *req.uri_mut() = format!("/wt/ping?token={}", ticket.token).parse().unwrap();
    // A non-WT listener must not consume a CONNECT ticket.
    assert!(evaluate(&policy, &req, peer(), true, Listener::default()).is_ok());
    bearer(&mut req, &cli);
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, listener),
        Ok(Authorization::Authenticated(_))
    ));
    assert!(store.consume_ticket(&ticket.token, target, PUBLIC).is_none());
    req.headers_mut().remove(header::AUTHORIZATION);
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, listener),
        Err(Refusal::AuthenticationRequired)
    ));

    let ticket = store
        .mint_ticket(&lease, PUBLIC, target, PUBLIC, Kind::WebTransport)
        .unwrap();
    *req.uri_mut() = format!("/wt/upload?token={}", ticket.token).parse().unwrap();
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, listener),
        Err(Refusal::AuthenticationRequired)
    ));
    assert!(store.consume_ticket(&ticket.token, target, PUBLIC).is_none());

    // An authenticated CONNECT from a foreign origin is forbidden, as in Go, not sent to sign in.
    *req.uri_mut() = "/wt/ping".parse().unwrap();
    set(&mut req, header::ORIGIN, CLIENT);
    bearer(&mut req, &cli);
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, listener),
        Err(Refusal::Forbidden)
    ));
}

#[test]
fn auth_pages_are_canonical_and_foreign_preflights_never_allow_cookies() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let ui = Listener {
        ui: true,
        webtransport: false,
    };
    let mut req = request("GET", "/login");
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, ui),
        Ok(Authorization::PublicAuth)
    ));
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, Listener::default()),
        Err(Refusal::Forbidden)
    ));
    set(&mut req, header::HOST, "meter.example:8443");
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, ui),
        Err(Refusal::Forbidden)
    ));
    req = request("OPTIONS", "/upload");
    set(&mut req, header::ORIGIN, CLIENT);
    set(&mut req, header::ACCESS_CONTROL_REQUEST_METHOD, "POST");
    req.headers_mut().insert(
        header::ACCESS_CONTROL_REQUEST_HEADERS,
        "Authorization, Content-Type".parse().unwrap(),
    );
    let Ok(Authorization::Preflight(headers)) = evaluate(&policy, &req, peer(), true, ui) else {
        panic!("expected bearer preflight");
    };
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], CLIENT);
    assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
    set(&mut req, header::ACCESS_CONTROL_REQUEST_HEADERS, "Content-Type");
    refused(&policy, &req, Refusal::Forbidden);
    *req.uri_mut() = "/auth/browser/token".parse().unwrap();
    let Ok(Authorization::Preflight(headers)) = evaluate(&policy, &req, peer(), true, ui) else {
        panic!("expected browser token preflight");
    };
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], CLIENT);
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_METHODS], "POST");
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_HEADERS], "Content-Type");
    assert_eq!(headers[header::ACCESS_CONTROL_MAX_AGE], "7200");
    assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
}

fn evaluate(
    policy: &Policy,
    request: &Request<()>,
    peer: SocketAddr,
    tls: bool,
    listener: Listener,
) -> Result<Authorization, Refusal> {
    policy
        .authorize(request.clone(), Connection { peer, tls, listener })
        .map(|authorized| authorized.authorization().clone())
        .map_err(|rejected| rejected.reason())
}

#[test]
fn repeated_security_headers_are_forbidden_before_anything_else() {
    let store = SessionStore::new();
    let policy = policy(&store);
    for name in [
        "authorization",
        "origin",
        "sec-fetch-site",
        "x-csrf-token",
        "access-control-request-method",
        "access-control-request-headers",
    ] {
        let mut req = request("POST", "/auth/password");
        req.headers_mut().append(name, "a".parse().unwrap());
        req.headers_mut().append(name, "b".parse().unwrap());
        refused(&policy, &req, Refusal::Ambiguous);
    }
}

#[test]
fn only_the_two_sign_in_fonts_are_public_and_only_for_get_and_head() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let listener = Listener {
        ui: true,
        webtransport: false,
    };
    refused(&policy, &request("HEAD", "/login"), Refusal::AuthenticationRequired);
    for path in [
        "/fonts/ibm-plex-sans-var-latin1.woff2",
        "/fonts/ibm-plex-mono-600-latin1.woff2",
    ] {
        for method in ["GET", "HEAD", "POST", "OPTIONS", "DELETE"] {
            let result = evaluate(&policy, &request(method, path), peer(), true, listener);
            assert_eq!(result.is_ok(), matches!(method, "GET" | "HEAD"), "{method} {path}");
        }
    }
    for path in [
        "/fonts/ibm-plex-sans-var-latin2.woff2",
        "/fonts/ibm-plex-mono-500-latin1.woff2",
        "/fonts/ibm-plex-sans-var-latin1.woff2/",
        "/fonts/",
    ] {
        refused(&policy, &request("GET", path), Refusal::AuthenticationRequired);
    }
}

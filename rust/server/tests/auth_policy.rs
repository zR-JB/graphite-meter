use graphite_meter_server::{
    auth::{
        AuthLease, SessionStore, SocketKind,
        policy::{Authorization, Connection, Listener, Policy, Refusal},
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

fn cookie(request: &mut Request<()>, token: &str) {
    request.headers_mut().insert(
        header::COOKIE,
        format!("__Host-gm_session={token}").parse().unwrap(),
    );
}

fn bearer(request: &mut Request<()>, token: &str) {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
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
    req.headers_mut()
        .insert(header::HOST, "meter.example:8443".parse().unwrap());
    let trust = policy.trust(&req, peer(), true);
    assert!(trust.secure && !trust.canonical);
    req.headers_mut()
        .insert(header::HOST, "other.example".parse().unwrap());
    assert!(!policy.trust(&req, peer(), true).secure);
    req.headers_mut()
        .insert("x-forwarded-proto", "https".parse().unwrap());
    req.headers_mut()
        .insert("x-forwarded-host", "meter.example".parse().unwrap());
    assert!(!policy.trust(&req, peer(), false).secure);
    assert!(policy.trust(&req, trusted, false).canonical);
    req.headers_mut()
        .append("x-forwarded-proto", "https".parse().unwrap());
    assert!(!policy.trust(&req, trusted, false).secure);
    req.headers_mut()
        .insert("x-forwarded-proto", "https,http".parse().unwrap());
    assert!(!policy.trust(&req, trusted, false).secure);
    req.headers_mut()
        .insert("x-real-ip", "::ffff:198.51.100.4".parse().unwrap());
    assert_eq!(
        policy
            .client_address(req.headers(), trusted)
            .unwrap()
            .to_string(),
        "198.51.100.4"
    );
    req.headers_mut()
        .insert("x-forwarded-for", "198.51.100.4".parse().unwrap());
    assert!(policy.client_address(req.headers(), trusted).is_none());
    assert_eq!(
        policy.client_address(req.headers(), peer()),
        Some(peer().ip())
    );
}

#[test]
fn cookie_measurements_require_positive_origin_evidence_and_mutation_csrf() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let (token, session) = store.create("operator", "Operator", "local", None).unwrap();
    let mut req = request("GET", "/download");
    cookie(&mut req, &token);
    refused(&policy, &req, Refusal::Forbidden);
    req.headers_mut()
        .insert("sec-fetch-site", "same-origin".parse().unwrap());
    assert!(!allowed(&policy, &req).is_bearer());
    req.headers_mut()
        .insert("sec-fetch-site", "same-site".parse().unwrap());
    refused(&policy, &req, Refusal::Forbidden);
    req.headers_mut()
        .insert(header::ORIGIN, PUBLIC.parse().unwrap());
    allowed(&policy, &req);
    *req.method_mut() = http::Method::POST;
    *req.uri_mut() = "/upload".parse().unwrap();
    refused(&policy, &req, Refusal::Forbidden);
    req.headers_mut()
        .insert("x-csrf-token", session.session().csrf().parse().unwrap());
    allowed(&policy, &req);
    req.headers_mut()
        .insert(header::ORIGIN, CLIENT.parse().unwrap());
    refused(&policy, &req, Refusal::Forbidden);

    *req.method_mut() = http::Method::GET;
    *req.uri_mut() = "/ws/ping".parse().unwrap();
    req.headers_mut().remove(header::ORIGIN);
    req.headers_mut()
        .insert("sec-fetch-site", "same-origin".parse().unwrap());
    refused(&policy, &req, Refusal::Forbidden);
    req.headers_mut()
        .insert(header::ORIGIN, PUBLIC.parse().unwrap());
    allowed(&policy, &req);
}

#[test]
fn explicit_credentials_never_fall_back_to_ambient_cookies() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let (token, session) = store.create("operator", "Operator", "local", None).unwrap();
    let (cli, _) = store.issue_cli_grant(&session).unwrap();
    let mut req = request("GET", "/download");
    cookie(&mut req, &token);
    req.headers_mut()
        .insert(header::ORIGIN, PUBLIC.parse().unwrap());
    bearer(&mut req, "invalid");
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
fn browser_grants_are_audience_and_route_scoped() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let (_, session) = store.create("operator", "Operator", "local", None).unwrap();
    let (token, _) = store.issue_browser_grant(&session, CLIENT).unwrap();
    let mut req = request("POST", "/upload");
    bearer(&mut req, &token);
    refused(&policy, &req, Refusal::Forbidden);
    req.headers_mut()
        .insert(header::ORIGIN, CLIENT.parse().unwrap());
    assert_eq!(allowed(&policy, &req).browser_origin(), Some(CLIENT));
    for path in ["/servers", "/auth/session", "/", "/unknown"] {
        *req.uri_mut() = path.parse().unwrap();
        refused(&policy, &req, Refusal::Forbidden);
    }
    *req.uri_mut() = "/upload".parse().unwrap();
    req.headers_mut()
        .insert(header::ORIGIN, PUBLIC.parse().unwrap());
    refused(&policy, &req, Refusal::Forbidden);
}

#[test]
fn webtransport_uses_no_cookie_and_burns_tickets_even_with_bearer() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let (token, session) = store.create("operator", "Operator", "local", None).unwrap();
    let lease = AuthLease::cookie(session.clone());
    let (cli, _) = store.issue_cli_grant(&session).unwrap();
    let target = "https://meter.example/wt/ping";
    let listener = Listener {
        ui: false,
        webtransport: true,
    };
    let mut req = request("CONNECT", "/wt/ping");
    cookie(&mut req, &token);
    req.headers_mut()
        .insert(header::ORIGIN, PUBLIC.parse().unwrap());
    req.headers_mut()
        .insert("x-csrf-token", session.session().csrf().parse().unwrap());
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, listener),
        Err(Refusal::AuthenticationRequired)
    ));
    let ticket = store
        .mint_ticket(&lease, PUBLIC, target, PUBLIC, SocketKind::WebTransport)
        .unwrap();
    *req.uri_mut() = format!("/wt/ping?token={}", ticket.token).parse().unwrap();
    // A non-WT listener must not consume a CONNECT ticket.
    assert!(evaluate(&policy, &req, peer(), true, Listener::default()).is_ok());
    bearer(&mut req, &cli);
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, listener),
        Ok(Authorization::Authenticated(_))
    ));
    assert!(
        store
            .consume_ticket(&ticket.token, target, PUBLIC)
            .is_none()
    );
    req.headers_mut().remove(header::AUTHORIZATION);
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, listener),
        Err(Refusal::AuthenticationRequired)
    ));

    let ticket = store
        .mint_ticket(&lease, PUBLIC, target, PUBLIC, SocketKind::WebTransport)
        .unwrap();
    *req.uri_mut() = format!("/wt/upload?token={}", ticket.token)
        .parse()
        .unwrap();
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, listener),
        Err(Refusal::AuthenticationRequired)
    ));
    assert!(
        store
            .consume_ticket(&ticket.token, target, PUBLIC)
            .is_none()
    );
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
    req.headers_mut()
        .insert(header::HOST, "meter.example:8443".parse().unwrap());
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, ui),
        Err(Refusal::Forbidden)
    ));
    req = request("OPTIONS", "/upload");
    req.headers_mut()
        .insert(header::ORIGIN, CLIENT.parse().unwrap());
    req.headers_mut().insert(
        header::ACCESS_CONTROL_REQUEST_METHOD,
        "POST".parse().unwrap(),
    );
    req.headers_mut().insert(
        header::ACCESS_CONTROL_REQUEST_HEADERS,
        "Authorization, Content-Type".parse().unwrap(),
    );
    let Ok(Authorization::Preflight(headers)) = evaluate(&policy, &req, peer(), true, ui) else {
        panic!("expected bearer preflight");
    };
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], CLIENT);
    assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
    req.headers_mut().insert(
        header::ACCESS_CONTROL_REQUEST_HEADERS,
        "Content-Type".parse().unwrap(),
    );
    refused(&policy, &req, Refusal::Forbidden);
    *req.uri_mut() = "/auth/browser/token".parse().unwrap();
    assert!(matches!(
        evaluate(&policy, &req, peer(), true, ui),
        Ok(Authorization::Preflight(_))
    ));
}

fn evaluate(
    policy: &Policy,
    request: &Request<()>,
    peer: SocketAddr,
    tls: bool,
    listener: Listener,
) -> Result<Authorization, Refusal> {
    policy
        .authorize(
            request.clone(),
            Connection {
                peer,
                tls,
                listener,
            },
        )
        .map(|authorized| authorized.authorization().clone())
        .map_err(|rejected| rejected.reason())
}

#[tokio::test]
async fn revoked_authorization_never_activates_or_leaves_active_work_running() {
    use graphite_meter_server::auth::policy::SessionEnded;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let store = SessionStore::new();
    let policy = policy(&store);
    let (token, session) = store.create("operator", "Operator", "local", None).unwrap();
    let mut req = request("GET", "/download");
    cookie(&mut req, &token);
    req.headers_mut()
        .insert(header::ORIGIN, PUBLIC.parse().unwrap());
    let connection = Connection {
        peer: peer(),
        tls: true,
        listener: Listener::default(),
    };
    let authorized = policy.authorize(req.clone(), connection).ok().unwrap();
    store.revoke(&session);
    let invoked = AtomicUsize::new(0);
    assert_eq!(
        authorized
            .run(async |_| {
                invoked.fetch_add(1, Ordering::SeqCst);
            })
            .await,
        Err(SessionEnded)
    );
    assert_eq!(invoked.load(Ordering::SeqCst), 0);

    let (token, session) = store.create("operator", "Operator", "local", None).unwrap();
    cookie(&mut req, &token);
    let authorized = policy.authorize(req, connection).ok().unwrap();
    struct Active(Arc<AtomicUsize>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let active = Arc::new(AtomicUsize::new(0));
    let count = active.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(authorized.run(async move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        let _active = Active(count);
        started.send(()).unwrap();
        std::future::pending::<()>().await;
    }));
    ready.await.unwrap();
    store.revoke(&session);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap(),
        Err(SessionEnded)
    );
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[test]
fn authorized_metadata_and_resolved_route_survive_body_conversion() {
    let store = SessionStore::new();
    let policy = policy(&store);
    let (token, _) = store.create("operator", "Operator", "local", None).unwrap();
    let mut req = request("POST", "/%75pload");
    cookie(&mut req, &token);
    req.headers_mut()
        .insert(header::ORIGIN, PUBLIC.parse().unwrap());
    let connection = Connection {
        peer: peer(),
        tls: true,
        listener: Listener::default(),
    };
    let authorized = policy
        .authorize(req, connection)
        .ok()
        .unwrap()
        .map_body(|()| vec![1, 2, 3]);
    assert_eq!(authorized.request().uri().path(), "/%75pload");
    assert_eq!(
        authorized.measurement_route(),
        None,
        "dispatch must not decode an unchecked route into /upload"
    );
    assert_eq!(authorized.request().headers()[header::ORIGIN], PUBLIC);
    assert_eq!(authorized.request().body(), &[1, 2, 3]);
}

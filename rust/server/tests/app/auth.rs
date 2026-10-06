//! Authentication over the app: Go's refusals, trust, and cookie and grant access.

use super::*;
use graphite_meter_server::{
    auth::{NewLogin, OPERATOR, Store},
    limits::CONNECTION_CREDIT,
    peer::ClientKeys,
};
use http::{HeaderValue, StatusCode};

pub(super) const PUBLIC: &str = "https://meter.example";
pub(super) const BROWSER: &str = "https://app.example";
/// `api/password.testvectors.json`: the operator's password and Go's hash of it.
pub(super) static VECTORS: std::sync::LazyLock<serde_json::Value> = std::sync::LazyLock::new(|| {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../api/password.testvectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
});
pub(super) static HASH: std::sync::LazyLock<&str> = std::sync::LazyLock::new(|| VECTORS["hash"].as_str().unwrap());

pub(super) fn auth_app(env: &[(&str, &str)]) -> App {
    let mut env = [ALL_LISTENERS.as_slice(), env].concat();
    env.extend([
        ("GM_AUTH_MODE", "password"),
        ("GM_AUTH_PUBLIC_URL", PUBLIC),
        ("GM_AUTH_PASSWORD_HASH", *HASH),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "http1-tls,http2,http3"),
    ]);
    app(&env)
}

pub(super) fn store(app: &App) -> &Store {
    app.auth().store().unwrap()
}

pub(super) fn login(app: &App, subject: &str) -> NewLogin {
    store(app).sign_in(subject, "Local operator", "local").unwrap()
}

/// A request naming the public host.
pub(super) fn public(method: &str, uri: &str) -> Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "meter.example")
}

/// A request from the public origin with the cookie of `login`, proving its CSRF token.
pub(super) fn signed(method: &str, uri: &str, login: &NewLogin) -> Builder {
    public(method, uri)
        .header("origin", PUBLIC)
        .header("cookie", format!("theme=dark; __Host-gm_session={}", login.token))
        .header("x-csrf-token", &login.csrf)
}

pub(super) fn bearer(method: &str, uri: &str, grant: &str) -> Builder {
    public(method, uri).header("authorization", format!("Bearer {grant}"))
}

pub(super) async fn tls<B: http_body::Body>(app: &App, request: Request<B>) -> Response<Body> {
    send(app, Endpoint::H1Tls, request).await
}

pub(super) fn assert_headers(response: &Response<Body>, expected: &[(&str, Option<&str>)]) {
    for (name, value) in expected {
        assert_eq!(header(response, name), *value, "{name}");
    }
}

#[tokio::test]
async fn an_unauthenticated_request_gets_gos_refusal_and_the_app_root_a_redirect() {
    let app = auth_app(&[]);
    let refused = tls(&app, empty(public("GET", "/download?bytes=1"))).await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert_headers(
        &refused,
        &[
            ("graphite-meter-auth", Some("required")),
            ("graphite-meter-auth-url", Some("https://meter.example/login")),
            ("graphite-meter-browser-auth", Some("1")),
            ("connection", Some("close")),
            ("cache-control", Some("no-store")),
            ("x-frame-options", Some("DENY")),
            ("strict-transport-security", Some("max-age=31536000")),
            ("access-control-allow-origin", None),
        ],
    );
    let policy = header(&refused, "content-security-policy").unwrap();
    assert!(policy.starts_with("default-src 'none'; style-src 'sha256-"), "{policy}");
    let root = tls(&app, empty(public("GET", "/"))).await;
    assert_eq!(root.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(header(&root, "location"), Some("https://meter.example/login"));
    assert_eq!(text(root).await, "<a href=\"https://meter.example/login\">Temporary Redirect</a>.\n\n");

    let from_ui = tls(&app, empty(public("GET", "/probe").header("origin", PUBLIC))).await;
    assert_headers(
        &from_ui,
        &[
            ("access-control-allow-origin", Some(PUBLIC)),
            ("access-control-allow-credentials", Some("true")),
        ],
    );
    let measuring = tls(&app, empty(public("GET", "/probe").header("origin", BROWSER))).await;
    assert_headers(
        &measuring,
        &[("access-control-allow-origin", Some(BROWSER)), ("access-control-allow-credentials", None)],
    );
    let twice = public("GET", "/probe")
        .header("origin", PUBLIC)
        .header("origin", PUBLIC);
    let repeated = tls(&app, empty(twice)).await;
    assert_eq!(
        (repeated.status(), header(&repeated, "graphite-meter-auth")),
        (StatusCode::FORBIDDEN, None)
    );
}

#[tokio::test]
async fn only_https_at_the_public_hostname_is_trusted() {
    let app = auth_app(&[("GM_TRUSTED_PROXIES", "10.0.0.0/8")]);
    let login = login(&app, "operator");
    let foreign = tls(
        &app,
        empty(request("GET", "/probe").header("cookie", format!("__Host-gm_session={}", login.token))),
    )
    .await;
    assert_headers(
        &foreign,
        &[("graphite-meter-auth", Some("required")), ("strict-transport-security", None)],
    );
    let direct = send(&app, Endpoint::H1, empty(signed("GET", "/probe", &login))).await;
    assert_headers(&direct, &[("graphite-meter-auth", Some("required")), ("strict-transport-security", None)]);
    let root = send(&app, Endpoint::H1, empty(public("GET", "/"))).await;
    assert_eq!(
        root.status(),
        StatusCode::TEMPORARY_REDIRECT,
        "the clear listener sends the app root to HTTPS"
    );
    let forwarded = |proto: &str| {
        signed("GET", "/probe", &login)
            .header("x-forwarded-proto", proto)
            .header("x-forwarded-host", "meter.example")
            .header("x-real-ip", "192.0.2.9")
    };
    let proxied = send_from(&app, Endpoint::H1, "10.0.0.2", empty(forwarded("https"))).await;
    assert_eq!(proxied.status(), StatusCode::OK);
    let plain = send_from(&app, Endpoint::H1, "10.0.0.2", empty(forwarded("http"))).await;
    assert_eq!(plain.status(), StatusCode::FORBIDDEN);
    let spoofed = send_from(&app, Endpoint::H1, "192.0.2.1", empty(forwarded("https"))).await;
    assert_eq!(spoofed.status(), StatusCode::FORBIDDEN, "only a trusted proxy forwards HTTPS");
}

#[tokio::test]
async fn a_cookie_login_measures_from_the_public_origin_and_proves_csrf_for_writes() {
    let app = auth_app(&[]);
    let login = login(&app, "operator");
    let probe = tls(&app, empty(signed("GET", "/probe", &login))).await;
    assert_eq!(probe.status(), StatusCode::OK);
    assert_headers(
        &probe,
        &[
            ("access-control-allow-origin", Some(PUBLIC)),
            ("access-control-allow-credentials", Some("true")),
            ("vary", Some("Origin")),
        ],
    );
    let session = tls(&app, empty(signed("POST", "/upload/session", &login))).await;
    assert_eq!(session.status(), StatusCode::OK);
    let mut unproved = signed("POST", "/upload/session", &login);
    unproved.headers_mut().unwrap().remove("x-csrf-token");
    assert_eq!(
        tls(&app, empty(unproved)).await.status(),
        StatusCode::FORBIDDEN,
        "a write proves its token"
    );
    let cross = signed("GET", "/probe", &login).header("sec-fetch-site", "cross-site");
    assert_eq!(tls(&app, empty(cross)).await.status(), StatusCode::FORBIDDEN);
    let mut elsewhere = signed("GET", "/probe", &login);
    elsewhere
        .headers_mut()
        .unwrap()
        .insert("origin", HeaderValue::from_static(BROWSER));
    assert_eq!(tls(&app, empty(elsewhere)).await.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn grants_measure_only_where_they_may_and_end_with_their_login() {
    let app = auth_app(&[]);
    let login = login(&app, "operator");
    let browser = store(&app)
        .grant(login.key, Some(HeaderValue::from_static(BROWSER)))
        .unwrap();
    let probe = tls(&app, empty(bearer("GET", "/probe", &browser).header("origin", BROWSER))).await;
    assert_eq!(probe.status(), StatusCode::OK);
    assert_headers(
        &probe,
        &[("access-control-allow-origin", Some(BROWSER)), ("access-control-allow-credentials", None)],
    );
    for (path, origin) in [("/servers", BROWSER), ("/probe", PUBLIC), ("/probe", ""), ("/", BROWSER)] {
        let refused = tls(&app, empty(bearer("GET", path, &browser).header("origin", origin))).await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN, "{path} from {origin:?}");
    }
    let native = store(&app).grant(login.key, None).unwrap();
    assert_eq!(tls(&app, empty(bearer("GET", "/servers", &native))).await.status(), StatusCode::OK);
    let mint = bearer("POST", "/ws/session?target=https://meter.example/ws/ping", &native);
    assert_eq!(
        tls(&app, empty(mint)).await.status(),
        StatusCode::FORBIDDEN,
        "native grants open sockets directly"
    );
    assert!(store(&app).sign_out(login.key, false));
    let ended = tls(&app, empty(bearer("GET", "/probe", &native))).await;
    assert_eq!(header(&ended, "graphite-meter-auth"), Some("required"));
}

#[tokio::test]
async fn password_logins_fund_windows_past_one_client_s_share() {
    // Three logins at a client's share each pass the principal's double share unless it is exempt.
    let share = 8 * CONNECTION_CREDIT;
    let funded = |app: &App| {
        let held: Vec<_> = (0..3)
            .map(|_| {
                let token = store(app).sign_in(OPERATOR, "Local operator", "local").unwrap().token;
                app.window_credit(&store(app).cookie(&token).unwrap().keys(), share)
            })
            .collect();
        held.iter().flatten().count()
    };
    assert_eq!(funded(&auth_app(&[])), 3);
    assert_eq!(funded(&super::oidc::oidc_app("https://localhost:1", &[], false)), 2);
    let (open, source) = (app(&[]), ClientKeys::address("192.0.2.1".parse().unwrap()));
    let _held = open.window_credit(&source, share).unwrap();
    assert!(open.window_credit(&source, 1).is_none(), "without auth a client keeps its share");
}

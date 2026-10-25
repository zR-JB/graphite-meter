//! Tests sessions over the app, with the sockets a lease opens, sign-out and the upload ownership a login decides.

use super::{
    auth::{BROWSER, PUBLIC, auth_app, bearer, login, public, signed, store, tls},
    *,
};
use graphite_meter_proto::lease::UNUSED_BOUND;
use graphite_meter_server::{
    auth::{COUNTERS, NewLogin, Principal},
    transport::websocket::Socket,
};
use http::StatusCode;
use http_body_util::Full;
use serde_json::json;
use std::time::Duration;

const NONCE: &str = "dGhlIHNhbXBsZSBub25jZQ==";

fn websocket(path: &str, origin: &str) -> Request<Full<Bytes>> {
    let request = public("GET", path)
        .header("origin", origin)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", NONCE);
    empty(request)
}

async fn take(app: &App, login: &NewLogin, body: Value) -> Value {
    let request = signed("POST", "/lease", login).body(Full::new(Bytes::from(body.to_string())));
    json(tls(app, request.unwrap()).await).await
}

fn is_socket(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::WebSocket(..) | Outcome::WebTransport(..))
}

#[tokio::test(start_paused = true)]
async fn a_lease_authenticates_sockets_from_its_origin_until_unused_or_signed_out() {
    let app = auth_app(&[]);
    let login = login(&app, Principal::operator());
    let lease = take(&app, &login, json!({"measurements": 2, "sessions": 0})).await;
    let token = lease["token"].as_str().unwrap();
    assert!(token.starts_with("gml_"));
    let path = format!("/ws/ping?lease={token}");
    for _ in 0..3 {
        let opened = outcome(&app, Endpoint::H1Tls, "192.0.2.1", websocket(&path, PUBLIC)).await;
        assert!(is_socket(&opened), "a lease opens one socket after another");
    }
    let foreign = tls(&app, websocket(&path, BROWSER)).await;
    assert_eq!(header(&foreign, "graphite-meter-auth"), Some("required"));
    tokio::time::sleep(UNUSED_BOUND + Duration::from_millis(1)).await;
    let unused = tls(&app, websocket(&path, PUBLIC)).await;
    assert_eq!(header(&unused, "graphite-meter-auth"), Some("required"));
    let lease = take(&app, &login, json!({"measurements": 1, "sessions": 0})).await;
    let path = format!("/ws/ping?lease={}", lease["token"].as_str().unwrap());
    assert!(store(&app).sign_out(login.key, false));
    tokio::task::yield_now().await;
    let signed_out = tls(&app, websocket(&path, PUBLIC)).await;
    assert_eq!(header(&signed_out, "graphite-meter-auth"), Some("required"));
}

#[tokio::test]
async fn sign_out_proves_csrf_and_ends_the_login_or_every_login_of_its_subject() {
    let app = auth_app(&[]);
    let current = login(&app, Principal::operator());
    let report = json(tls(&app, empty(signed("GET", "/auth/session", &current))).await).await;
    assert_eq!(
        (report["name"].as_str(), report["provider"].as_str()),
        (Some("Local operator"), Some("local"))
    );
    assert_eq!(report["csrf"].as_str(), Some(current.csrf.as_str()));
    assert_eq!(report["maximumLifetimeMs"], 28_800_000);
    assert!(
        report["remainingMs"].as_u64().unwrap() > 28_700_000
            && report["expires"].as_str().unwrap().ends_with('Z')
    );
    for scope in ["", "all"] {
        let (current, sibling, other) = (
            login(&app, Principal::operator()),
            login(&app, Principal::operator()),
            login(&app, Principal::oidc("other")),
        );
        let logout = |csrf: &str| {
            let request = signed("POST", "/auth/logout", &current)
                .header("content-type", "application/x-www-form-urlencoded");
            request
                .body(Full::new(Bytes::from(format!("csrf={csrf}&scope={scope}"))))
                .unwrap()
        };
        assert_eq!(
            tls(&app, logout("forged-proof-of-the-csrf-token"))
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        let signed_out = tls(&app, logout(current.csrf.as_str())).await;
        assert_eq!(signed_out.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            header(&signed_out, "location"),
            Some("/login?reason=signed_out")
        );
        let cleared: Vec<_> = signed_out
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        let expired = "=; Path=/; Expires=Thu, 01 Jan 1970 00:00:01 GMT; Max-Age=0";
        assert_eq!(
            cleared,
            [
                format!("__Host-gm_session{expired}; HttpOnly; Secure; SameSite=Strict"),
                format!("__Host-gm_login{expired}; HttpOnly; Secure; SameSite=Strict"),
                format!("__Host-gm_csrf{expired}; Secure; SameSite=Strict"),
            ]
        );
        let store = store(&app);
        assert!(
            store.cookie(current.token.as_str()).is_none()
                && store.cookie(other.token.as_str()).is_some()
        );
        assert_eq!(
            store.cookie(sibling.token.as_str()).is_some(),
            scope.is_empty(),
            "scope {scope:?}"
        );
    }
    let minute = app
        .auth()
        .security()
        .unwrap()
        .line(&mut [0; COUNTERS])
        .unwrap();
    assert!(minute.contains(" logout=2 "), "{minute}");
}

async fn upload_id(app: &App, login: &NewLogin) -> String {
    let lease = take(
        app,
        login,
        json!({"measurements": 1, "sessions": 0, "uploads": 1}),
    )
    .await;
    lease["uploads"][0].as_str().unwrap().into()
}

#[tokio::test]
async fn an_upload_belongs_to_the_login_or_grant_that_started_it() {
    let app = auth_app(&[]);
    let (owner, sibling) = (
        login(&app, Principal::operator()),
        login(&app, Principal::operator()),
    );
    let id = upload_id(&app, &owner).await;
    let started = signed("POST", &format!("/upload?id={id}"), &owner)
        .body(Full::new(Bytes::from_static(b"abc")));
    assert_eq!(
        text(tls(&app, started.unwrap()).await).await,
        r#"{"bytes":3}"#
    );
    let checkpoint = format!("/upload/checkpoint?id={id}");
    let grant = store(&app)
        .grant(owner.key, None)
        .unwrap()
        .as_str()
        .to_owned();
    for refused in [
        signed("POST", &checkpoint, &sibling),
        bearer("POST", &checkpoint, &grant),
    ] {
        let refused = tls(&app, empty(refused)).await;
        assert_eq!(
            header(&refused, "graphite-meter-refusal"),
            Some("owner-mismatch")
        );
    }
    assert_eq!(
        json(tls(&app, empty(signed("POST", &checkpoint, &owner))).await).await["bytes"],
        3
    );
}

#[tokio::test]
async fn a_progress_socket_presenting_the_lease_attaches_its_feed() {
    let app = auth_app(&[]);
    let owner = login(&app, Principal::operator());
    let lease = take(
        &app,
        &owner,
        json!({"measurements": 1, "sessions": 0, "uploads": 1}),
    )
    .await;
    let (token, id) = (
        lease["token"].as_str().unwrap(),
        lease["uploads"][0].as_str().unwrap(),
    );
    let open = websocket(&format!("/ws/progress?id={id}&lease={token}"), PUBLIC);
    let opened = outcome(&app, Endpoint::H1Tls, "192.0.2.1", open).await;
    assert!(matches!(opened, Outcome::WebSocket(_, Socket::Feed(..))));
}

//! Sessions over the app: one-use socket tickets, sign-out and the upload ownership a login decides.

use super::{
    auth::{BROWSER, PUBLIC, auth_app, bearer, login, public, signed, store, tls},
    *,
};
use graphite_meter_proto::upload::Session;
use graphite_meter_server::auth::{COUNTERS, NewLogin};
use http::StatusCode;
use http_body_util::Full;
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

async fn mint(app: &App, login: &NewLogin, route: &str, target: &str) -> Response<Body> {
    tls(app, empty(signed("POST", &format!("{route}?target={target}"), login))).await
}

async fn ticket(app: &App, login: &NewLogin, route: &str, target: &str) -> String {
    let minted = json(mint(app, login, route, target).await).await;
    assert!(minted["expires"].as_u64().unwrap() > 0);
    minted["token"].as_str().unwrap().into()
}

fn is_socket(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::WebSocket(..) | Outcome::WebTransport(..))
}

#[tokio::test(start_paused = true)]
async fn socket_tickets_are_one_use_bound_to_target_and_origin_and_expire() {
    let app = auth_app(&[]);
    let login = login(&app, "operator");
    let target = "https://meter.example/ws/ping";
    let token = ticket(&app, &login, "/ws/session", target).await;
    assert!(token.starts_with("gmw_"));
    let path = format!("/ws/ping?token={token}");
    assert!(is_socket(&outcome(&app, Endpoint::H1Tls, "192.0.2.1", websocket(&path, PUBLIC)).await));
    let replayed = tls(&app, websocket(&path, PUBLIC)).await;
    assert_eq!(header(&replayed, "graphite-meter-auth"), Some("required"), "a ticket opens one socket");
    let path = format!("/ws/ping?token={}", ticket(&app, &login, "/ws/session", target).await);
    assert_eq!(tls(&app, websocket(&path, BROWSER)).await.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        tls(&app, websocket(&path, PUBLIC)).await.status(),
        StatusCode::FORBIDDEN,
        "a refusal spends it"
    );
    let path = format!("/ws/ping?token={}", ticket(&app, &login, "/ws/session", target).await);
    tokio::time::advance(Duration::from_secs(30)).await;
    assert_eq!(
        tls(&app, websocket(&path, PUBLIC)).await.status(),
        StatusCode::FORBIDDEN,
        "tickets expire"
    );
    for target in [
        "https://meter.example/ws/ping%3Fx",
        "https://meter.example/ws/ping%23x",
        "http://meter.example/ws/ping",
        "https://other.example/ws/ping",
        "https://user@meter.example/ws/ping",
        "https://meter.example/wt/ping",
    ] {
        assert_eq!(
            mint(&app, &login, "/ws/session", target).await.status(),
            StatusCode::BAD_REQUEST,
            "{target}"
        );
    }
    for _ in 0..8 {
        ticket(&app, &login, "/ws/session", "https://meter.example:8443/ws/ping").await;
    }
    let full = mint(&app, &login, "/ws/session", target).await;
    assert_eq!((full.status(), header(&full, "retry-after")), (StatusCode::TOO_MANY_REQUESTS, Some("1")));
}

#[tokio::test]
async fn sign_out_proves_csrf_and_ends_the_login_or_every_login_of_its_subject() {
    let app = auth_app(&[]);
    let current = login(&app, "operator");
    let report = json(tls(&app, empty(signed("GET", "/auth/session", &current))).await).await;
    assert_eq!(
        (report["name"].as_str(), report["provider"].as_str()),
        (Some("Local operator"), Some("local"))
    );
    assert_eq!(report["csrf"].as_str(), Some(current.csrf.as_str()));
    assert_eq!(report["maximumLifetimeMs"], 28_800_000);
    assert!(report["remainingMs"].as_u64().unwrap() > 28_700_000 && report["expires"].as_str().unwrap().ends_with('Z'));
    for scope in ["", "all"] {
        let (current, sibling, other) = (login(&app, "operator"), login(&app, "operator"), login(&app, "other"));
        let logout = |csrf: &str| {
            let request =
                signed("POST", "/auth/logout", &current).header("content-type", "application/x-www-form-urlencoded");
            request
                .body(Full::new(Bytes::from(format!("csrf={csrf}&scope={scope}"))))
                .unwrap()
        };
        assert_eq!(tls(&app, logout("forged-proof-of-the-csrf-token")).await.status(), StatusCode::FORBIDDEN);
        let signed_out = tls(&app, logout(&current.csrf)).await;
        assert_eq!(signed_out.status(), StatusCode::SEE_OTHER);
        assert_eq!(header(&signed_out, "location"), Some("/login?reason=signed_out"));
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
        assert!(store.cookie(&current.token).is_none() && store.cookie(&other.token).is_some());
        assert_eq!(store.cookie(&sibling.token).is_some(), scope.is_empty(), "scope {scope:?}");
    }
    let minute = app.auth().security().unwrap().line(&mut [0; COUNTERS]).unwrap();
    assert!(minute.contains(" logout=2 "), "{minute}");
}

async fn upload_id(app: &App, login: &NewLogin) -> String {
    let minted = tls(app, empty(signed("POST", "/upload/session", login))).await;
    Session::decode(text(minted).await.as_bytes()).unwrap().upload_id
}

#[tokio::test]
async fn an_upload_belongs_to_the_login_or_grant_that_started_it() {
    let app = auth_app(&[]);
    let (owner, sibling) = (login(&app, "operator"), login(&app, "operator"));
    let id = upload_id(&app, &owner).await;
    let started = signed("POST", &format!("/upload?id={id}"), &owner).body(Full::new(Bytes::from_static(b"abc")));
    assert_eq!(text(tls(&app, started.unwrap()).await).await, r#"{"bytes":3}"#);
    let checkpoint = format!("/upload/checkpoint?id={id}");
    let grant = store(&app).grant(owner.key, None).unwrap();
    for refused in [signed("POST", &checkpoint, &sibling), bearer("POST", &checkpoint, &grant)] {
        let refused = tls(&app, empty(refused)).await;
        assert_eq!(header(&refused, "x-graphite-upload-refusal"), Some("ownerMismatch"));
    }
    assert_eq!(json(tls(&app, empty(signed("POST", &checkpoint, &owner))).await).await["bytes"], 3);
}

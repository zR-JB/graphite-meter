//! Password sign-in over the app: the page, Go's cookies, refusals and their notices, the attempt budgets, renewal
//! and the security log's counts.

use super::{
    auth::{PUBLIC, assert_headers, auth_app, login, public, store, tls},
    *,
};
use graphite_meter_proto::approval::challenge;
use graphite_meter_server::auth::COUNTERS;
use http::StatusCode;
use http_body_util::Full;
use std::{pin::pin, time::Duration};

const NONCE: &str = "a-long-unpredictable-sign-in-nonce";
const RIGHT: &str = "correct+horse";

/// A password sign-in from `peer` with the form token `csrf`, the cookies `cookies` and further form `fields`.
async fn post(app: &App, peer: &str, csrf: &str, cookies: &str, fields: &str) -> Response<Body> {
    let request = public("POST", "/auth/password")
        .header("origin", PUBLIC)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", cookies)
        .body(Full::new(Bytes::from(format!("csrf={csrf}&{fields}"))))
        .unwrap();
    send_from(app, Endpoint::H1Tls, peer, request).await
}

/// A sign-in from `peer` whose form token its login cookie holds.
async fn sign_in(app: &App, peer: &str, password: &str) -> Response<Body> {
    post(app, peer, NONCE, &format!("__Host-gm_login={NONCE}"), &format!("password={password}")).await
}

fn location(response: &Response<Body>) -> Option<&str> {
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    header(response, "location")
}

fn line(app: &App, last: &mut [u64; COUNTERS]) -> Option<String> {
    app.auth().security().unwrap().line(last)
}

#[tokio::test]
async fn the_sign_in_page_and_a_right_password_set_gos_cookies_and_redirect() {
    let app = auth_app(&[]);
    let page = tls(&app, empty(public("GET", "/login?reason=renew&error=forged"))).await;
    assert_eq!(page.status(), StatusCode::OK);
    assert_headers(
        &page,
        &[
            ("content-type", Some("text/html; charset=utf-8")),
            ("cache-control", Some("no-store")),
            ("x-frame-options", Some("DENY")),
            ("strict-transport-security", Some("max-age=31536000")),
            ("referrer-policy", Some("same-origin")),
            ("x-content-type-options", Some("nosniff")),
            ("permissions-policy", Some("camera=(), microphone=(), geolocation=()")),
        ],
    );
    let policy = header(&page, "content-security-policy").unwrap();
    assert!(policy.starts_with("default-src 'none'; style-src 'sha256-") && policy.ends_with("base-uri 'none'"));
    let cookie = header(&page, "set-cookie").unwrap().to_owned();
    let nonce = cookie
        .strip_prefix("__Host-gm_login=")
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert!(cookie.ends_with("; Max-Age=599; HttpOnly; Secure; SameSite=Strict"), "{cookie}");
    let html = text(page).await;
    assert!(html.contains(&format!("<input type=\"hidden\" name=\"csrf\" value=\"{nonce}\">")));
    assert!(html.contains("Sign in again before starting this long test.") && html.contains("Sign-in failed."));

    let queried = public("POST", &format!("/auth/password?password={RIGHT}"))
        .header("origin", PUBLIC)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", format!("__Host-gm_login={nonce}"))
        .body(Full::new(Bytes::from(format!("csrf={nonce}"))))
        .unwrap();
    let queried = tls(&app, queried).await;
    assert_eq!(location(&queried), Some("/login?error=password"), "a password in the query is not read");
    let signed = post(&app, "192.0.2.1", nonce, &format!("__Host-gm_login={nonce}"), "password=correct+horse").await;
    assert_eq!(location(&signed), Some("/"));
    let cookies: Vec<_> = signed.headers().get_all("set-cookie").iter().collect();
    let cookies: Vec<_> = cookies.iter().map(|value| value.to_str().unwrap()).collect();
    let (session, attributes) = cookies[0].split_once("; Path=/; Expires=").unwrap();
    let (expires, attributes) = attributes.split_once("; ").unwrap();
    assert!(expires.ends_with(" GMT") && expires.len() == 29, "{expires}");
    assert_eq!(attributes, "Max-Age=28799; HttpOnly; Secure; SameSite=Strict");
    assert!(
        cookies[1].starts_with("__Host-gm_csrf=") && cookies[1].ends_with("; Max-Age=28799; Secure; SameSite=Strict")
    );
    let cleared = "__Host-gm_login=; Path=/; Expires=Thu, 01 Jan 1970 00:00:01 GMT; Max-Age=0; HttpOnly; Secure";
    assert_eq!(cookies[2], format!("{cleared}; SameSite=Strict"));
    assert!(cookies[3].starts_with("__Host-gm_device="));
    assert!(cookies[3].ends_with("; Max-Age=2591999; HttpOnly; Secure; SameSite=Strict"));
    assert_eq!(cookies.len(), 4);
    let token = session.strip_prefix("__Host-gm_session=").unwrap();
    assert!(store(&app).cookie(token).is_some(), "the cookie holds the new login");
    assert!(
        line(&app, &mut [0; COUNTERS])
            .unwrap()
            .starts_with("[gm:auth] 1m local=1 oidc=0 invalid-password=1 ")
    );
}

#[tokio::test(start_paused = true)]
async fn wrong_passwords_are_refused_counted_and_throttled_until_the_minute_passes() {
    let app = auth_app(&[]);
    let mut last = [0; COUNTERS];
    let stale = post(&app, "192.0.2.1", NONCE, "", "password=correct+horse").await;
    assert_eq!(location(&stale), Some("/login?error=stale"));
    let forged = post(
        &app,
        "192.0.2.1",
        "another-long-unpredictable-nonce",
        &format!("__Host-gm_login={NONCE}"),
        "",
    )
    .await;
    assert_eq!(location(&forged), Some("/login?error=failed"));
    for _ in 0..5 {
        assert_eq!(location(&sign_in(&app, "192.0.2.1", "wrong").await), Some("/login?error=password"));
    }
    let counts = "local=0 oidc=0 invalid-password=5 oidc-failure=0 group-denial=0 replay-expiry=0 throttled=0 logout=0 \
                  cli-approval=0 capacity=0";
    assert_eq!(line(&app, &mut last), Some(format!("[gm:auth] 1m {counts}")));
    assert_eq!(location(&sign_in(&app, "192.0.2.1", RIGHT).await), Some("/login?error=throttled"));
    let approval = challenge("a-verifier-that-never-leaves-the-requester");
    let fields = format!("password={RIGHT}&challenge={approval}");
    let throttled = post(&app, "192.0.2.1", NONCE, &format!("__Host-gm_login={NONCE}"), &fields).await;
    assert_eq!(
        location(&throttled),
        Some(format!("/login?challenge={approval}&error=throttled").as_str())
    );
    assert_eq!(location(&sign_in(&app, "192.0.2.2", RIGHT).await), Some("/"), "another client's budget");
    tokio::time::advance(Duration::from_secs(60)).await;
    let signed = post(&app, "192.0.2.1", NONCE, &format!("__Host-gm_login={NONCE}"), &fields).await;
    assert_eq!(location(&signed), Some(format!("/auth/cli?challenge={approval}").as_str()));
    let minute = line(&app, &mut last).unwrap();
    assert!(minute.starts_with("[gm:auth] 1m local=2 oidc=0 invalid-password=0 ") && minute.contains(" throttled=2 "));
    assert_eq!(line(&app, &mut last), None, "an unchanged minute writes no line");
}

#[tokio::test]
async fn signing_in_again_ends_the_presented_login_and_its_grants() {
    let app = auth_app(&[]);
    let (prior, sibling) = (login(&app, "local-operator"), login(&app, "local-operator"));
    let grant = store(&app).grant(prior.key, None).unwrap();
    let cookies = format!("__Host-gm_login={NONCE}; __Host-gm_session={}", prior.token);
    let renewed = post(&app, "192.0.2.1", NONCE, &cookies, "password=correct+horse").await;
    assert_eq!(location(&renewed), Some("/"));
    let store = store(&app);
    assert!(store.cookie(&prior.token).is_none() && store.bearer(&grant).is_none());
    assert!(store.cookie(&sibling.token).is_some());
}

#[tokio::test]
async fn a_full_store_refuses_sign_in_as_busy_and_counts_it() {
    let app = auth_app(&[]);
    for subject in 0..1024 {
        login(&app, &format!("subject {subject}"));
    }
    assert_eq!(location(&sign_in(&app, "192.0.2.1", RIGHT).await), Some("/login?error=busy"));
    assert!(
        line(&app, &mut [0; COUNTERS])
            .unwrap()
            .ends_with(" logout=0 cli-approval=0 capacity=1")
    );
}

#[tokio::test]
async fn the_password_is_verified_off_the_runtime() {
    let app = auth_app(&[]);
    let mut signing = pin!(sign_in(&app, "192.0.2.1", RIGHT));
    let mut turns = 0;
    let signed = loop {
        tokio::select! {
            biased;
            signed = &mut signing => break signed,
            () = tokio::task::yield_now() => turns += 1,
        }
    };
    assert_eq!(location(&signed), Some("/"));
    assert!(turns > 0, "the runtime's one thread served nothing else while the hash was verified");
}

/// A sign-in from `peer` posting `body` as `media`, with the login cookie its form token proves.
async fn post_as(app: &App, peer: &str, media: &str, body: String) -> Response<Body> {
    let request = public("POST", "/auth/password")
        .header("origin", PUBLIC)
        .header("content-type", media)
        .header("cookie", format!("__Host-gm_login={NONCE}"))
        .body(Full::new(Bytes::from(body)))
        .unwrap();
    send_from(app, Endpoint::H1Tls, peer, request).await
}

#[tokio::test]
async fn a_sign_in_reads_a_form_of_at_most_4_kib_with_unique_fields() {
    let app = auth_app(&[]);
    const FORM: &str = "application/x-www-form-urlencoded";
    let padded = |length: usize| {
        let fields = format!("csrf={NONCE}&password={RIGHT}&pad=");
        format!("{fields}{}", "x".repeat(length - fields.len()))
    };
    let largest = post_as(&app, "192.0.2.1", FORM, padded(4096)).await;
    assert_eq!(location(&largest), Some("/"));
    let failed = Some("/login?error=failed");
    assert_eq!(location(&post_as(&app, "192.0.2.2", FORM, padded(4097)).await), failed);
    let repeated = format!("csrf={NONCE}&csrf={NONCE}&password={RIGHT}");
    assert_eq!(location(&post_as(&app, "192.0.2.3", FORM, repeated).await), failed);
    let json = format!(r#"{{"csrf":"{NONCE}","password":"correct horse"}}"#);
    assert_eq!(location(&post_as(&app, "192.0.2.4", "application/json", json).await), failed);
    let boundary = "multipart/form-data; boundary=x";
    let parts = format!(
        "--x\r\nContent-Disposition: form-data; name=\"csrf\"\r\n\r\n{NONCE}\r\n--x\r\nContent-Disposition: form-data; \
         name=\"password\"\r\n\r\ncorrect horse\r\n--x--\r\n"
    );
    assert_eq!(location(&post_as(&app, "192.0.2.5", boundary, parts).await), failed);
}

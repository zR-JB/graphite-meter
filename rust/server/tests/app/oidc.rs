//! OIDC sign-in over the app against a fake provider over TLS: the round trip, transaction binding, ID token and
//! user information checks, group membership, discovery, budgets and the security log.

use super::{
    auth::{HASH, PUBLIC, public, store, tls},
    provider::{CLIENT_ID, Provider, SECRET, Twist, child},
    *,
};
use graphite_meter_proto::approval::challenge;
use graphite_meter_server::{app::query, auth::COUNTERS, peer::ClientKeys};
use http::StatusCode;
use http_body_util::Full;
use serde_json::json;
use std::{
    pin::pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const NONCE: &str = "a-long-unpredictable-sign-in-nonce";

fn oidc_app(issuer: &str, hybrid: bool) -> App {
    let mut env = ALL_LISTENERS.to_vec();
    env.extend([
        ("GM_AUTH_MODE", if hybrid { "hybrid" } else { "oidc" }),
        ("GM_AUTH_PUBLIC_URL", PUBLIC),
        ("GM_AUTH_OIDC_ISSUER", issuer),
        ("GM_AUTH_OIDC_CLIENT_ID", CLIENT_ID),
        ("GM_AUTH_OIDC_CLIENT_SECRET", SECRET),
        ("GM_AUTH_OIDC_ALLOWED_GROUPS", "admins,operators"),
        ("GM_AUTH_OIDC_PROVIDER_NAME", "Id"),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "http1-tls,http2,http3"),
    ]);
    if hybrid {
        env.push(("GM_AUTH_PASSWORD_HASH", HASH));
    }
    app(&env)
}

async fn discovered(provider: &Provider) -> App {
    let app = oidc_app(provider.issuer(), false);
    app.auth().discover().await.unwrap();
    app
}

/// The start form posted from `peer` with a valid CSRF proof and `challenge`.
async fn start(app: &App, peer: &str, challenge: &str) -> Response<Body> {
    let request = public("POST", "/auth/oidc/start")
        .header("origin", PUBLIC)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", format!("__Host-gm_login={NONCE}"))
        .body(Full::new(Bytes::from(format!("csrf={NONCE}&challenge={challenge}"))))
        .unwrap();
    send_from(app, Endpoint::H1Tls, peer, request).await
}

async fn callback(app: &App, peer: &str, query: &str, cookie: &str) -> Response<Body> {
    let request = public("GET", &format!("/auth/oidc/callback?{query}")).header("cookie", cookie);
    send_from(app, Endpoint::H1Tls, peer, empty(request)).await
}

fn location(response: &Response<Body>) -> &str {
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    header(response, "location").unwrap()
}

/// The `Set-Cookie` line of `name`.
fn set_cookie<'a>(response: &'a Response<Body>, name: &str) -> &'a str {
    let lines = response.headers().get_all("set-cookie").iter();
    let mut lines = lines.map(|line| line.to_str().unwrap());
    lines.find(|line| line.starts_with(&format!("{name}="))).unwrap()
}

fn cookie_value<'a>(response: &'a Response<Body>, name: &str) -> &'a str {
    let line = set_cookie(response, name);
    line[name.len() + 1..].split(';').next().unwrap()
}

/// A sign-in started from `peer` whose callback comes from `returning`.
async fn sign_in_from(app: &App, provider: &Provider, peer: &str, returning: &str) -> Response<Body> {
    let started = start(app, peer, "").await;
    let browser = cookie_value(&started, "__Host-gm_oidc").to_owned();
    let (code, state) = provider.authorize(location(&started));
    let query = format!("code={code}&state={state}&iss={}", query::escape(provider.issuer()));
    callback(app, returning, &query, &format!("__Host-gm_oidc={browser}")).await
}

async fn sign_in(app: &App, provider: &Provider, peer: &str) -> Response<Body> {
    sign_in_from(app, provider, peer, peer).await
}

fn line(app: &App, last: &mut [u64; COUNTERS]) -> String {
    app.auth().security().unwrap().line(last).unwrap()
}

#[tokio::test]
async fn a_sign_in_runs_from_the_form_through_the_provider_to_the_login_s_cookies() {
    if !child("oidc::a_sign_in_runs_from_the_form_through_the_provider_to_the_login_s_cookies") {
        return;
    }
    let provider = Provider::start().await;
    let app = discovered(&provider).await;
    let page = tls(&app, empty(public("GET", "/login"))).await;
    let form_action = format!("form-action 'self' {}; ", provider.issuer());
    assert!(header(&page, "content-security-policy").unwrap().contains(&form_action));
    let html = text(page).await;
    let button = "<button type=\"submit\" >Continue with Id</button>";
    assert!(html.contains(button) && !html.contains("current-password"));

    let approval = challenge("a-verifier-that-never-leaves-the-terminal");
    let started = start(&app, "192.0.2.1", &approval).await;
    assert!(
        header(&started, "content-security-policy")
            .unwrap()
            .contains(&form_action)
    );
    let transaction = set_cookie(&started, "__Host-gm_oidc");
    assert!(transaction.ends_with("; Max-Age=599; HttpOnly; Secure; SameSite=Lax"), "{transaction}");
    let browser = cookie_value(&started, "__Host-gm_oidc").to_owned();
    let (code, state) = provider.authorize(location(&started));
    let query = format!("code={code}&state={state}&iss={}", query::escape(provider.issuer()));
    let signed = callback(&app, "192.0.2.1", &query, &format!("__Host-gm_oidc={browser}")).await;
    assert_eq!(signed.status(), StatusCode::OK);
    let cleared = "__Host-gm_oidc=; Path=/; Expires=Thu, 01 Jan 1970 00:00:01 GMT; Max-Age=0; HttpOnly; Secure";
    assert_eq!(set_cookie(&signed, "__Host-gm_oidc"), format!("{cleared}; SameSite=Lax"));
    assert!(set_cookie(&signed, "__Host-gm_session").ends_with("; Max-Age=28799; HttpOnly; Secure; SameSite=Strict"));
    assert!(set_cookie(&signed, "__Host-gm_csrf").ends_with("; Max-Age=28799; Secure; SameSite=Strict"));
    assert!(set_cookie(&signed, "__Host-gm_login").contains("Max-Age=0;"));
    let token = cookie_value(&signed, "__Host-gm_session").to_owned();
    let html = text(signed).await;
    assert!(
        html.contains(&format!("<a href=\"/auth/cli?challenge={approval}\">Continue</a>")),
        "{html}"
    );

    let lease = store(&app).cookie(&token).unwrap();
    assert!(matches!(lease.keys(), ClientKeys::Auth(_, principal) if &*principal == "oidc:operator"));
    let report = public("GET", "/auth/session").header("cookie", format!("__Host-gm_session={token}"));
    let report = json(tls(&app, empty(report)).await).await;
    assert_eq!((&report["name"], &report["provider"]), (&json!("Example Operator"), &json!("Id")));
    assert!(
        line(&app, &mut [0; COUNTERS]).starts_with("[gm:auth] 1m local=0 oidc=1 invalid-password=0 oidc-failure=0 ")
    );
    assert_eq!(provider.key_sets(), 1, "the key set is fetched with the first token");
}

#[tokio::test]
async fn callbacks_whose_state_cookie_issuer_verifier_or_nonce_do_not_match_are_refused() {
    if !child("oidc::callbacks_whose_state_cookie_issuer_verifier_or_nonce_do_not_match_are_refused") {
        return;
    }
    let provider = Provider::start().await;
    let app = discovered(&provider).await;
    let issuer = query::escape(provider.issuer());
    let failed = "/login?error=failed";
    let begin = async |peer| {
        let started = start(&app, peer, "").await;
        let cookie = format!("__Host-gm_oidc={}", cookie_value(&started, "__Host-gm_oidc"));
        let (code, state) = provider.authorize(location(&started));
        (code, state, cookie)
    };
    let refused = callback(&app, "192.0.2.1", "error=access_denied&code=c&state=s", "").await;
    assert_eq!(location(&refused), failed);
    let (code, state, cookie) = begin("192.0.2.1").await;
    let query = format!("code={code}&state={state}&iss={issuer}");
    assert_eq!(location(&callback(&app, "192.0.2.1", &query, "").await), "/login?error=stale");
    assert_eq!(location(&callback(&app, "192.0.2.1", &query, "__Host-gm_oidc=forged").await), failed);
    assert_eq!(
        location(&callback(&app, "192.0.2.1", &query, &cookie).await),
        failed,
        "the forgery spent it"
    );

    let (code, state, cookie) = begin("192.0.2.2").await;
    let elsewhere = format!("code={code}&state={state}&iss=https%3A%2F%2Felsewhere.example");
    assert_eq!(location(&callback(&app, "192.0.2.2", &elsewhere, &cookie).await), failed);
    let (code, state, cookie) = begin("192.0.2.2").await;
    let unnamed = format!("code={code}&state={state}");
    assert_eq!(
        location(&callback(&app, "192.0.2.2", &unnamed, &cookie).await),
        failed,
        "the provider names itself"
    );

    let (first, _, _) = begin("192.0.2.3").await;
    let (_, state, cookie) = begin("192.0.2.3").await;
    let swapped = format!("code={first}&state={state}&iss={issuer}");
    assert_eq!(
        location(&callback(&app, "192.0.2.3", &swapped, &cookie).await),
        failed,
        "another verifier"
    );
    provider.twist(Twist {
        claims: json!({"nonce": "another-sign-in"}),
        ..Twist::default()
    });
    assert_eq!(location(&sign_in(&app, &provider, "192.0.2.4").await), failed);
    provider.twist(Twist::default());
    assert_eq!(sign_in(&app, &provider, "192.0.2.4").await.status(), StatusCode::OK);
    let counts = "oidc=1 invalid-password=0 oidc-failure=8 group-denial=0 replay-expiry=2 throttled=0";
    assert!(line(&app, &mut [0; COUNTERS]).contains(counts));
}

#[tokio::test]
async fn a_login_needs_an_allowed_group_and_the_id_token_s_subject() {
    if !child("oidc::a_login_needs_an_allowed_group_and_the_id_token_s_subject") {
        return;
    }
    let provider = Provider::start().await;
    let app = discovered(&provider).await;
    for userinfo in [
        json!({"groups": ["Operators", "guests"]}),
        json!({"groups": null}),
        json!({"sub": "another"}),
    ] {
        provider.twist(Twist { userinfo, ..Twist::default() });
        assert_eq!(location(&sign_in(&app, &provider, "192.0.2.1").await), "/login?error=failed");
    }
    provider.twist(Twist {
        userinfo: json!({"groups": ["guests", "admins"]}),
        ..Twist::default()
    });
    assert_eq!(sign_in(&app, &provider, "192.0.2.1").await.status(), StatusCode::OK);
    assert!(line(&app, &mut [0; COUNTERS]).contains("oidc=1 invalid-password=0 oidc-failure=3 group-denial=2 "));
}

#[tokio::test]
async fn id_tokens_need_an_allowed_algorithm_a_known_key_and_valid_claims() {
    if !child("oidc::id_tokens_need_an_allowed_algorithm_a_known_key_and_valid_claims") {
        return;
    }
    let provider = Provider::start().await;
    let app = discovered(&provider).await;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let mut peer = 0;
    let mut signs_in = async |header, claims| {
        provider.twist(Twist { header, claims, ..Twist::default() });
        peer += 1;
        sign_in(&app, &provider, &format!("192.0.2.{peer}")).await.status() == StatusCode::OK
    };
    for header in [
        json!({"alg": "RS256", "kid": "rsa"}),
        json!({"alg": "RS512", "kid": null}),
        json!({"alg": "PS256", "kid": "rsa"}),
        json!({"alg": "PS384", "kid": "rsa"}),
        json!({"alg": "ES384", "kid": "p384"}),
        json!({"alg": "EdDSA", "kid": "ed", "typ": "application/jwt"}),
    ] {
        assert!(signs_in(header.clone(), json!({})).await, "{header}");
    }
    for claims in [
        json!({"aud": ["another", CLIENT_ID]}),
        json!({"at_hash": null, "iat": null}),
        json!({"nbf": now + 200}),
    ] {
        assert!(signs_in(json!({}), claims.clone()).await, "{claims}");
    }
    for header in [
        json!({"alg": "HS256"}),
        json!({"alg": "none"}),
        json!({"alg": "ES512"}),
        json!({"alg": "RS256", "kid": "p256"}),
        json!({"crit": ["exp"]}),
        json!({"typ": "secevent+jwt"}),
    ] {
        assert!(!signs_in(header.clone(), json!({})).await, "{header}");
    }
    let refreshed = provider.key_sets();
    assert!(!signs_in(json!({"kid": "stranger"}), json!({})).await);
    assert_eq!(provider.key_sets(), refreshed + 1, "an unknown key refetches the key set once");
    for claims in [
        json!({"iss": "https://elsewhere.example"}),
        json!({"aud": "another"}),
        json!({"exp": now - 1}),
        json!({"exp": null}),
        json!({"nbf": now + 400}),
        json!({"at_hash": "another-access-token"}),
        json!({"nonce": null}),
        json!({"name": 5}),
    ] {
        assert!(!signs_in(json!({}), claims.clone()).await, "{claims}");
    }
}

#[tokio::test]
async fn hybrid_keeps_the_password_while_the_provider_is_down_and_discovers_it_in_the_background() {
    if !child("oidc::hybrid_keeps_the_password_while_the_provider_is_down_and_discovers_it_in_the_background") {
        return;
    }
    let provider = Provider::start().await;
    provider.set_ready(false);
    let app = oidc_app(provider.issuer(), true);
    let page = async || text(tls(&app, empty(public("GET", "/login"))).await).await;
    let html = page().await;
    assert!(html.contains("<button type=\"submit\" disabled>Continue with Id</button>"));
    assert!(html.contains("<p class=\"notice\">Id is unavailable right now.</p>") && html.contains("current-password"));
    assert_eq!(location(&start(&app, "192.0.2.1", "").await), "/login?error=provider");
    let html = text(tls(&app, empty(public("GET", "/login?error=provider"))).await).await;
    assert!(html.contains("Id is unavailable right now. Sign in with the operator password.</p>"));
    let password = public("POST", "/auth/password")
        .header("origin", PUBLIC)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", format!("__Host-gm_login={NONCE}"))
        .body(Full::new(Bytes::from(format!("csrf={NONCE}&password=correct+horse"))))
        .unwrap();
    assert_eq!(location(&tls(&app, password).await), "/");

    let discovery = app.auth().background_discovery().unwrap();
    let ready = async {
        while !page()
            .await
            .contains("<button type=\"submit\" >Continue with Id</button>")
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
            provider.set_ready(true);
        }
    };
    tokio::select! {
        () = discovery => unreachable!("discovery runs until the server stops"),
        ready = tokio::time::timeout(Duration::from_secs(5), ready) => ready.unwrap(),
    }
    assert_eq!(sign_in(&app, &provider, "192.0.2.2").await.status(), StatusCode::OK);
    assert!(
        line(&app, &mut [0; COUNTERS]).starts_with("[gm:auth] 1m local=1 oidc=1 invalid-password=0 oidc-failure=1 ")
    );
}

#[tokio::test]
async fn oidc_mode_without_its_provider_shows_the_provider_notice_and_discovers_only_at_startup() {
    let app = oidc_app("https://localhost:1", false);
    assert!(app.auth().background_discovery().is_none());
    let html = text(tls(&app, empty(public("GET", "/login"))).await).await;
    assert!(html.contains("disabled>Continue with Id</button>") && !html.contains("current-password"));
    assert_eq!(location(&start(&app, "192.0.2.1", "").await), "/login?error=provider");
    let html = text(tls(&app, empty(public("GET", "/login?error=provider"))).await).await;
    assert!(html.contains("Id is unavailable right now.</p>") && !html.contains("operator password"));
    let policy = header(&tls(&app, empty(public("GET", "/login"))).await, "content-security-policy")
        .unwrap()
        .to_owned();
    assert!(policy.contains("form-action 'self'; "), "no provider origin before discovery");
}

#[tokio::test]
async fn a_provider_signing_only_with_algorithms_this_server_cannot_verify_is_refused_at_discovery() {
    let test = "oidc::a_provider_signing_only_with_algorithms_this_server_cannot_verify_is_refused_at_discovery";
    if !child(test) {
        return;
    }
    let provider = Provider::start().await;
    provider.twist(Twist {
        metadata: json!({"id_token_signing_alg_values_supported": ["ES512", "HS256"]}),
        ..Twist::default()
    });
    let refused = oidc_app(provider.issuer(), false).auth().discover().await;
    let message = "provider advertises no ID token algorithm this server verifies: [\"ES512\", \"HS256\"]";
    assert_eq!(refused.unwrap_err(), message);
    let app = oidc_app(provider.issuer(), true);
    let discovery = app.auth().background_discovery().unwrap();
    let retrying = tokio::time::timeout(Duration::from_millis(1200), discovery).await;
    assert!(retrying.is_err(), "hybrid retries past the first backoff");
    let html = text(tls(&app, empty(public("GET", "/login"))).await).await;
    assert!(html.contains("disabled>Continue with Id</button>") && html.contains("current-password"));
}

#[tokio::test]
async fn starts_and_code_exchanges_are_budgeted_per_client_address() {
    if !child("oidc::starts_and_code_exchanges_are_budgeted_per_client_address") {
        return;
    }
    let provider = Provider::start().await;
    let app = discovered(&provider).await;
    let mut last = [0; COUNTERS];
    for _ in 0..8 {
        assert!(location(&start(&app, "192.0.2.1", "").await).starts_with(provider.issuer()));
    }
    for _ in 0..2 {
        assert_eq!(
            location(&start(&app, "192.0.2.1", "").await),
            "/login?error=busy",
            "eight open transactions"
        );
    }
    assert_eq!(location(&start(&app, "192.0.2.1", "").await), "/login?error=throttled");
    assert!(
        line(&app, &mut last)
            .contains("oidc-failure=3 group-denial=0 replay-expiry=0 throttled=1 logout=0 cli-approval=0 capacity=2")
    );
    for client in 10..20 {
        let signed = sign_in_from(&app, &provider, &format!("192.0.2.{client}"), "198.51.100.1").await;
        assert_eq!(signed.status(), StatusCode::OK);
    }
    let throttled = sign_in_from(&app, &provider, "192.0.2.20", "198.51.100.1").await;
    assert_eq!(location(&throttled), "/login?error=failed", "the eleventh exchange in the minute");
    assert!(line(&app, &mut last).starts_with("[gm:auth] 1m local=0 oidc=10 invalid-password=0 oidc-failure=1 "));
}

#[tokio::test]
async fn a_provider_too_slow_for_the_exchange_bound_fails_the_sign_in_in_time() {
    if !child("oidc::a_provider_too_slow_for_the_exchange_bound_fails_the_sign_in_in_time") {
        return;
    }
    let provider = Provider::start().await;
    let app = discovered(&provider).await;
    let started = start(&app, "192.0.2.1", "").await;
    let cookie = format!("__Host-gm_oidc={}", cookie_value(&started, "__Host-gm_oidc"));
    let (code, state) = provider.authorize(location(&started));
    let mut held = provider.hold();
    let query = format!("code={code}&state={state}&iss={}", query::escape(provider.issuer()));
    let mut signing = pin!(callback(&app, "192.0.2.1", &query, &cookie));
    let mut arrive = async |path| {
        let (arrived, release) = tokio::select! {
            held = held.recv() => held.unwrap(),
            _ = &mut signing => panic!("answered before {path}"),
        };
        assert_eq!(arrived, path);
        release
    };
    let elapse = async |duration| {
        tokio::time::pause();
        tokio::time::advance(duration).await;
        tokio::time::resume();
    };
    let token = arrive("/token").await;
    elapse(Duration::from_secs(7)).await;
    token.send(()).unwrap();
    let _userinfo = arrive("/userinfo").await;
    elapse(Duration::from_millis(5500)).await;
    let failed = tokio::time::timeout(Duration::from_secs(1), signing).await;
    let failed = failed.expect("answered while the 15 s exchange bound leaves time to send it");
    assert_eq!(location(&failed), "/login?error=failed");
    assert!(line(&app, &mut [0; COUNTERS]).contains(" oidc=0 invalid-password=0 oidc-failure=1 "));
}

//! OIDC sign-in over the app against a fake provider over TLS: the round trip, transaction binding, ID token and
//! key checks, group membership, discovery and the exchange bound.

use super::{
    auth::{HASH, PUBLIC, public, store, tls},
    provider::{CLIENT_ID, Provider, SECRET, Twist, child},
    *,
};
use graphite_meter_proto::approval::challenge;
use graphite_meter_server::{app::query, auth::COUNTERS, peer::ClientKeys};
use http::StatusCode;
use http_body_util::Full;
use serde_json::{Value, json};
use std::{
    pin::pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const NONCE: &str = "a-long-unpredictable-sign-in-nonce";

pub(super) fn oidc_app(issuer: &str, hybrid: bool) -> App {
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
        line(&app, &mut [0; COUNTERS])
            .starts_with("sign-ins in the last minute: local=0 oidc=1 invalid-password=0 oidc-failure=0 ")
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
async fn a_callback_needs_one_code_one_state_and_at_most_one_issuer_and_ignores_other_pairs() {
    if !child("oidc::a_callback_needs_one_code_one_state_and_at_most_one_issuer_and_ignores_other_pairs") {
        return;
    }
    let provider = Provider::start().await;
    let app = discovered(&provider).await;
    let issuer = query::escape(provider.issuer());
    for (extra, signs_in) in [
        ("&session_state=a&session_state=b&bad=%zz&odd;pair&&scope", true),
        ("&code=another", false),
        ("&state=", false),
        (&format!("&iss={issuer}"), false),
    ] {
        let started = start(&app, "192.0.2.1", "").await;
        let cookie = format!("__Host-gm_oidc={}", cookie_value(&started, "__Host-gm_oidc"));
        let (code, state) = provider.authorize(location(&started));
        let query = format!("code={code}&state={state}&iss={issuer}{extra}");
        let answer = callback(&app, "192.0.2.1", &query, &cookie).await;
        assert_eq!(answer.status() == StatusCode::OK, signs_in, "{extra}");
    }
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

/// Whether a sign-in through a fresh app, with no keys cached, passes once the provider answers with `twist`.
async fn signs_in_with(provider: &Provider, twist: Twist) -> bool {
    provider.twist(twist);
    let app = discovered(provider).await;
    sign_in(&app, provider, "192.0.2.1").await.status() == StatusCode::OK
}

fn signed_as(header: Value) -> Twist {
    Twist { header, ..Twist::default() }
}

/// An RS256 token from the RSA key whose key set entry merges `rsa_key`.
fn rsa_key(rsa_key: Value) -> Twist {
    Twist { rsa_key, ..signed_as(json!({"alg": "RS256", "kid": "rsa"})) }
}

fn signed_userinfo(userinfo: Value) -> Twist {
    Twist { signed_userinfo: true, userinfo, ..Twist::default() }
}

fn key_set(key_set: fn(Value) -> String, header: Value) -> Twist {
    Twist { key_set: Some(key_set), header, ..Twist::default() }
}

/// Sixty-three usable keys ahead of the provider's, leaving the RSA key the 64th and the P-256 key past the cap.
fn crowded(keys: Value) -> String {
    let filler = json!({"kty": "EC", "crv": "P-256", "kid": "filler", "x": "A".repeat(43), "y": "A".repeat(43)});
    let mut all = vec![filler; 63];
    all.extend(keys["keys"].as_array().unwrap().iter().cloned());
    json!({ "keys": all }).to_string()
}

#[tokio::test]
async fn token_key_and_key_set_rules_decide_a_fresh_sign_in() {
    if !child("oidc::token_key_and_key_set_rules_decide_a_fresh_sign_in") {
        return;
    }
    let provider = Provider::start().await;
    let rsa = || json!({"alg": "RS256", "kid": "rsa"});
    let padded = |header| Twist { padded_keys: true, ..signed_as(header) };
    // The claims beside the padding take about 250 bytes, and the header and signature about 150 encoded.
    let pad = |pad: usize| Twist { claims: json!({"pad": "x".repeat(pad)}), ..Twist::default() };
    let claims = |claims| Twist { claims, ..Twist::default() };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let rows = [
        ("RS256", signed_as(rsa()), true),
        ("PS256", signed_as(json!({"alg": "PS256", "kid": "rsa"})), true),
        ("EdDSA", signed_as(json!({"alg": "EdDSA", "kid": "ed", "typ": "application/jwt"})), true),
        ("HS256", signed_as(json!({"alg": "HS256"})), false),
        ("none", signed_as(json!({"alg": "none"})), false),
        ("ES512", signed_as(json!({"alg": "ES512"})), false),
        ("another key's kid", signed_as(json!({"alg": "RS256", "kid": "p256"})), false),
        ("crit", signed_as(json!({"crit": ["exp"]})), false),
        ("an audience list", claims(json!({"aud": ["another", CLIENT_ID]})), true),
        ("not before within the skew", claims(json!({"nbf": now + 200})), true),
        ("another issuer", claims(json!({"iss": "https://elsewhere.example"})), false),
        ("another audience", claims(json!({"aud": "another"})), false),
        ("expired", claims(json!({"exp": now - 1})), false),
        ("another access token", claims(json!({"at_hash": "another-access-token"})), false),
        ("no nonce", claims(json!({"nonce": null})), false),
        ("padded RSA", padded(rsa()), true),
        ("use=enc", rsa_key(json!({"use": "enc"})), false),
        ("key_ops=[sign]", rsa_key(json!({"key_ops": ["sign"]})), false),
        ("a key for RS384", rsa_key(json!({"alg": "RS384"})), false),
        ("16 KiB", pad(11_700), true),
        ("over 16 KiB", pad(12_100), false),
        ("cty", signed_as(json!({"cty": "JWT"})), false),
        ("signed user information", signed_userinfo(json!({})), true),
        (
            "user information from another issuer",
            signed_userinfo(json!({"iss": "https://elsewhere.example"})),
            false,
        ),
        ("user information for another client", signed_userinfo(json!({"aud": "another"})), false),
        (
            "keys named twice",
            key_set(|keys| format!(r#"{{"keys":[],"keys":{}}}"#, keys["keys"]), json!({})),
            false,
        ),
        ("the 64th usable key", key_set(crowded, rsa()), true),
        ("the 65th", key_set(crowded, json!({})), false),
    ];
    for (row, twist, accepted) in rows {
        assert_eq!(signs_in_with(&provider, twist).await, accepted, "{row}");
    }
    provider.twist(Twist::default());
    let app = discovered(&provider).await;
    assert_eq!(sign_in(&app, &provider, "192.0.2.1").await.status(), StatusCode::OK);
    let fetched = provider.key_sets();
    provider.twist(signed_as(json!({"kid": "stranger"})));
    assert_ne!(sign_in(&app, &provider, "192.0.2.2").await.status(), StatusCode::OK);
    assert_eq!(provider.key_sets(), fetched + 1, "an unknown key refetches the key set once");
}

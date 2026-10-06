//! Sign-in approvals over the app: the browser page, approval and verifier exchanges bound to their origin.

use super::{
    auth::{BROWSER, auth_app, login, public, signed, store, tls},
    *,
};
use graphite_meter_proto::approval::challenge;
use graphite_meter_server::auth::NewLogin;
use http::{HeaderValue, StatusCode};
use http_body_util::Full;

const VERIFIER: &str = "a-verifier-that-never-leaves-the-requester";

fn location(response: &Response<Body>) -> &str {
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    header(response, "location").unwrap()
}

async fn page(app: &App, path: &str, login: Option<&NewLogin>) -> Response<Body> {
    let request = match login {
        Some(login) => signed("GET", path, login),
        None => public("GET", path),
    };
    tls(app, empty(request)).await
}

async fn approve(app: &App, path: &str, login: &NewLogin, challenge: &str) -> Response<Body> {
    let request = signed("POST", path, login)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Full::new(Bytes::from(format!("csrf={}&challenge={challenge}", login.csrf))))
        .unwrap();
    tls(app, request).await
}

/// A token exchange at `path` for `verifier`, from `origin` when given.
async fn exchange(app: &App, path: &str, origin: Option<&str>, document: &str) -> Response<Body> {
    let mut request = public("POST", path).header("content-type", "application/json");
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    tls(app, request.body(Full::new(Bytes::from(document.to_owned()))).unwrap()).await
}

fn verifier(verifier: &str) -> String {
    format!("{{\"verifier\":\"{verifier}\"}}")
}

async fn pending(response: Response<Body>) -> bool {
    response.status() == StatusCode::ACCEPTED && text(response).await == r#"{"status":"pending"}"#
}

#[tokio::test]
async fn a_browser_approval_binds_its_grant_to_the_requesting_origin() {
    let app = auth_app(&[]);
    let operator = login(&app, "operator");
    let approval = challenge(VERIFIER);
    let path = format!("/auth/browser?challenge={approval}&client_origin=https%3A%2F%2Fapp.example");
    assert_eq!(location(&page(&app, &path, None).await), format!("/login?challenge={approval}"));
    let navigated = public("GET", &path)
        .header("sec-fetch-site", "cross-site")
        .header("sec-fetch-mode", "navigate")
        .header("sec-fetch-dest", "document");
    let html = text(tls(&app, empty(navigated)).await).await;
    assert!(html.contains("Continue sign-in") && html.contains(&format!("href=\"/auth/cli?challenge={approval}\"")));
    let terminal = page(&app, &format!("/auth/cli?challenge={approval}"), None).await;
    assert_eq!(location(&terminal), path, "the terminal page sends a browser challenge to its own page");

    let shown = page(&app, &path, Some(&operator)).await;
    assert_eq!(shown.status(), StatusCode::OK);
    let html = text(shown).await;
    assert!(html.contains("<strong>https://app.example</strong>") && html.contains("action=\"/auth/browser/approve\""));
    let elsewhere = page(&app, &path.replace("app.example", "other.example"), Some(&operator)).await;
    assert_eq!(elsewhere.status(), StatusCode::FORBIDDEN);
    let token = "/auth/browser/token";
    assert!(
        pending(exchange(&app, token, Some(BROWSER), &verifier(VERIFIER)).await).await,
        "not yet approved"
    );
    assert_eq!(
        approve(&app, "/auth/cli/approve", &operator, &approval).await.status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        text(approve(&app, "/auth/browser/approve", &operator, &approval).await)
            .await
            .contains("Browser client approved")
    );

    let other = exchange(&app, token, Some("https://other.example"), &verifier(VERIFIER)).await;
    assert_eq!(header(&other, "access-control-allow-origin"), Some("https://other.example"));
    assert!(pending(other).await, "another origin's exchange");
    assert!(
        pending(exchange(&app, "/auth/cli/token", None, &verifier(VERIFIER)).await).await,
        "a native exchange"
    );
    for (origin, document) in [
        (None, verifier(VERIFIER)),
        (Some("http://app.example"), verifier(VERIFIER)),
        (Some("https://app.example:443"), verifier(VERIFIER)),
        (Some(BROWSER), verifier(&VERIFIER[..31])),
        (Some(BROWSER), "{}".to_owned()),
    ] {
        assert_eq!(
            exchange(&app, token, origin, &document).await.status(),
            StatusCode::FORBIDDEN,
            "{origin:?} {document}"
        );
    }
    let issued = exchange(&app, token, Some(BROWSER), &verifier(VERIFIER)).await;
    assert_eq!(header(&issued, "access-control-allow-origin"), Some(BROWSER));
    assert_eq!(header(&issued, "access-control-allow-credentials"), None);
    let issued = json(issued).await;
    assert!(issued["expires"].as_u64().unwrap() > 1_700_000_000_000);
    assert!(issued["remainingMs"].as_u64().unwrap() > 28_700_000);
    assert_eq!(issued["maximumLifetimeMs"], 28_800_000);
    let grant = store(&app).bearer(issued["token"].as_str().unwrap()).unwrap();
    assert_eq!(grant.browser(), Some(&HeaderValue::from_static(BROWSER)));
}

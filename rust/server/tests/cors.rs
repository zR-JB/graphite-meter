use graphite_meter_core::route::Route;
use graphite_meter_server::cors::{Access, authenticated_preflight};
use http::{HeaderMap, HeaderValue, header};

fn request(origin: &str, method: &str, headers: &str) -> HeaderMap {
    let mut request = HeaderMap::new();
    request.insert(header::ORIGIN, origin.parse().unwrap());
    request.insert(
        header::ACCESS_CONTROL_REQUEST_METHOD,
        method.parse().unwrap(),
    );
    request.insert(
        header::ACCESS_CONTROL_REQUEST_HEADERS,
        headers.parse().unwrap(),
    );
    request
}

#[test]
fn cookie_and_bearer_preflights_have_distinct_credential_boundaries() {
    let public = HeaderValue::from_static("https://meter.example");
    let same_origin = request(
        "https://meter.example",
        "GET",
        "authorization, x-csrf-token",
    );
    let access = authenticated_preflight(&public, true, Some(Route::Download), &same_origin)
        .expect("canonical origin may use cookies and CSRF headers");
    let mut response = HeaderMap::new();
    response.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    access.apply_measurement(&mut response);
    assert_eq!(response[header::ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");
    assert_eq!(response[header::ACCESS_CONTROL_ALLOW_ORIGIN], public);
    assert_eq!(response.get_all(header::VARY).iter().count(), 2);

    let foreign = request("https://ui.example", "GET", "Authorization, Content-Type");
    let access = authenticated_preflight(&public, true, Some(Route::Download), &foreign)
        .expect("foreign origin may attempt a bearer-authenticated request");
    access.apply_measurement(&mut response);
    assert!(!response.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
    assert_eq!(
        response[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://ui.example"
    );
    assert_eq!(
        response[header::ACCESS_CONTROL_ALLOW_HEADERS],
        "Authorization, Content-Type"
    );
}

#[test]
fn refused_preflights_cannot_authorize_a_browser_request() {
    let public = HeaderValue::from_static("https://meter.example");
    for (origin, method, headers, secure, route) in [
        (
            "https://meter.example",
            "GET",
            "",
            false,
            Some(Route::Download),
        ),
        (
            "https://meter.example",
            "DELETE",
            "",
            true,
            Some(Route::Download),
        ),
        (
            "https://meter.example",
            "GET",
            "x-evil",
            true,
            Some(Route::Download),
        ),
        ("https://meter.example", "GET", "", true, None),
        ("https://ui.example", "GET", "", true, Some(Route::Download)),
        (
            "http://ui.example",
            "GET",
            "authorization",
            true,
            Some(Route::Download),
        ),
        (
            "https://ui.example/",
            "GET",
            "authorization",
            true,
            Some(Route::Download),
        ),
        (
            "https://ui.example",
            "GET",
            "authorization,x-csrf-token",
            true,
            Some(Route::Download),
        ),
        (
            "https://ui.example",
            "GET",
            "authorization",
            true,
            Some(Route::Servers),
        ),
    ] {
        let request = request(origin, method, headers);
        assert!(
            authenticated_preflight(&public, secure, route, &request).is_none(),
            "unexpected permission for {origin} {method} {route:?} {headers:?} secure={secure}"
        );
    }
}

#[test]
fn public_measurements_expose_timing_without_cookies() {
    let mut headers = HeaderMap::new();
    Access::Public.apply_measurement(&mut headers);
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert_eq!(headers["timing-allow-origin"], "*");
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_HEADERS], "*");
    assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
}

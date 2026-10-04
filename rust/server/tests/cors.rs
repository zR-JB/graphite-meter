use graphite_meter_core::route::Route;
use graphite_meter_server::cors::authenticated_preflight;
use http::{HeaderMap, HeaderValue, header};

fn request(origin: &str, method: &str, headers: &str) -> HeaderMap {
    let mut request = HeaderMap::new();
    request.insert(header::ORIGIN, origin.parse().unwrap());
    request.insert(header::ACCESS_CONTROL_REQUEST_METHOD, method.parse().unwrap());
    request.insert(header::ACCESS_CONTROL_REQUEST_HEADERS, headers.parse().unwrap());
    request
}

#[test]
fn cookie_and_bearer_preflights_have_distinct_credential_boundaries() {
    let public = HeaderValue::from_static("https://meter.example");
    let same_origin = request("https://meter.example", "GET", "authorization, x-csrf-token");
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
    assert_eq!(response[header::ACCESS_CONTROL_ALLOW_ORIGIN], "https://ui.example");
    let allowed = &response[header::ACCESS_CONTROL_ALLOW_HEADERS];
    assert_eq!(allowed, "Authorization, Content-Type");
}

#[test]
fn refused_preflights_cannot_authorize_a_browser_request() {
    let public = HeaderValue::from_static("https://meter.example");
    const DOWNLOAD: Option<Route> = Some(Route::Download);
    const METER: &str = "https://meter.example";
    const UI: &str = "https://ui.example";
    for (origin, method, headers, secure, route) in [
        (METER, "GET", "", false, DOWNLOAD),
        (METER, "DELETE", "", true, DOWNLOAD),
        (METER, "GET", "x-evil", true, DOWNLOAD),
        (METER, "GET", "", true, None),
        (UI, "GET", "", true, DOWNLOAD),
        ("http://ui.example", "GET", "authorization", true, DOWNLOAD),
        ("https://ui.example/", "GET", "authorization", true, DOWNLOAD),
        (UI, "GET", "authorization,x-csrf-token", true, DOWNLOAD),
        (UI, "GET", "authorization", true, Some(Route::Servers)),
    ] {
        let request = request(origin, method, headers);
        assert!(
            authenticated_preflight(&public, secure, route, &request).is_none(),
            "unexpected permission for {origin} {method} {route:?} {headers:?} secure={secure}"
        );
    }
}

#[test]
fn repeated_preflight_fields_cannot_choose_a_more_privileged_interpretation() {
    let public = HeaderValue::from_static("https://meter.example");
    for name in [
        header::ORIGIN,
        header::ACCESS_CONTROL_REQUEST_METHOD,
        header::ACCESS_CONTROL_REQUEST_HEADERS,
    ] {
        let mut headers = request("https://meter.example", "POST", "Content-Type");
        let duplicate = headers.get(&name).unwrap().clone();
        headers.append(name, duplicate);
        let access = authenticated_preflight(&public, true, Some(Route::Upload), &headers);
        assert!(access.is_none(), "repeated preflight field was accepted");
    }
}

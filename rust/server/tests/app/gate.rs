//! Mounts, request-head rules, methods, CORS and hardening headers.

use super::*;
use graphite_meter_proto::route::Route;
use graphite_meter_server::app::finalize::{Access, harden};
use http::{HeaderMap, HeaderValue, StatusCode, Version};
use http_body_util::Full;

#[test]
fn each_endpoint_mounts_the_routes_of_its_listener() {
    let mounted = |endpoint: Endpoint| {
        let routes = Route::ALL.iter().filter(|route| endpoint.mounts(**route));
        routes.map(|route| route.path()).collect::<Vec<_>>().join(" ")
    };
    let ui = "/preflight /probe /download /upload /upload/session /upload/progress /wt/session /ws/session /ws/ping \
              /servers /upload/checkpoint";
    let h2 = "/probe /download /upload /upload/session /upload/progress /wt/session /upload/checkpoint";
    let companion = "/probe /upload/session /upload/progress /wt/session /upload/checkpoint";
    let quic = "/probe /download /upload /upload/session /upload/progress /wt/session /wt/download /wt/upload \
                /wt/ping /upload/checkpoint";
    let expected = [ui, ui, h2, companion, quic];
    assert_eq!(Endpoint::ALL.map(mounted), expected);
    assert_eq!(Endpoint::ALL.map(Endpoint::ui), [true, true, false, false, false]);
    assert_eq!(Endpoint::ALL.map(Endpoint::bootstrap), [false, false, false, true, false]);
}

#[tokio::test]
async fn unmounted_routes_are_not_found_and_mounted_ones_check_their_methods() {
    let app = app(&ALL_LISTENERS);
    for (endpoint, method, path) in [
        (Endpoint::H3Companion, "GET", "/download"),
        (Endpoint::H2, "GET", "/preflight"),
        (Endpoint::Quic, "GET", "/ws/ping"),
        (Endpoint::H2, "GET", "//probe"),
        (Endpoint::H2, "GET", "/probe/"),
    ] {
        let response = send(&app, endpoint, empty(request(method, path))).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{endpoint:?} {method} {path}");
        assert_eq!(header(&response, "access-control-allow-origin"), None);
        assert_eq!(text(response).await, "404 page not found\n");
    }
    for (endpoint, method, path, allow) in [
        (Endpoint::H2, "POST", "/probe", "GET, HEAD, OPTIONS"),
        (Endpoint::Quic, "DELETE", "/upload/session", "OPTIONS, POST"),
        (Endpoint::H3Companion, "PUT", "/upload/progress", "DELETE, GET, HEAD, OPTIONS"),
        (Endpoint::Quic, "GET", "/wt/upload", "CONNECT"),
    ] {
        let response = send(&app, endpoint, empty(request(method, path))).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
        assert_eq!(header(&response, "allow"), Some(allow));
    }
}

#[tokio::test]
async fn webtransport_sessions_are_admitted_before_their_upgrade() {
    let app = app(&[ALL_LISTENERS.as_slice(), &[("GM_MAX_SESSIONS_PER_CLIENT", "1")]].concat());
    let connect = async |path: &str| outcome(&app, Endpoint::Quic, "192.0.2.1", empty(request("CONNECT", path))).await;
    let Outcome::WebTransport(accepted, download, _) = connect("/wt/download").await else {
        panic!("a session");
    };
    assert_eq!(accepted.status(), StatusCode::OK);
    assert_eq!(header(&accepted, "access-control-allow-origin"), Some("*"));
    let Outcome::Response(refused) = connect("/wt/download").await else {
        panic!("a refusal");
    };
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS, "one transfer session per client");
    let Outcome::WebTransport(_, ping, _) = connect("/wt/ping").await else {
        panic!("a bus");
    };
    assert_eq!(active(&app).await, 2, "the bus is an operation beside the session");
    drop((download, ping));
    assert_eq!(active(&app).await, 0);
}

#[tokio::test]
async fn a_request_head_over_32_kib_is_refused() {
    let app = app(&[]);
    let fixed = "GET".len() + "/nope".len() + 14 + "host".len() + "speed.example".len() + 4 + "x-fill".len() + 4;
    for (fill, status) in [(0, StatusCode::NOT_FOUND), (1, StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)] {
        let value = "a".repeat((32 << 10) - fixed + fill);
        let response = send(&app, Endpoint::H1, empty(request("GET", "/nope").header("x-fill", value))).await;
        assert_eq!(response.status(), status);
    }
}

#[tokio::test]
async fn an_http11_request_names_exactly_one_valid_host() {
    let app = app(&ALL_LISTENERS);
    let bare = |version| Request::builder().uri("/nope").version(version);
    for (builder, refusal) in [
        (bare(Version::HTTP_11), "400 Bad Request: missing required Host header"),
        (
            bare(Version::HTTP_11).header("host", "speed example"),
            "400 Bad Request: malformed Host header",
        ),
        (bare(Version::HTTP_11).header("host", "a").header("host", "a"), "400 Bad Request"),
    ] {
        let response = send(&app, Endpoint::H1, empty(builder)).await;
        assert_eq!(
            (response.status(), header(&response, "connection")),
            (StatusCode::BAD_REQUEST, Some("close"))
        );
        assert_eq!(text(response).await, format!("{refusal}\n"));
    }
    let valid = [
        (Endpoint::H1, bare(Version::HTTP_10)),
        (Endpoint::H2, bare(Version::HTTP_2)),
        (Endpoint::H1, bare(Version::HTTP_11).header("host", "[fe80::1%25eth0]:7246")),
    ];
    for (endpoint, builder) in valid {
        assert_eq!(send(&app, endpoint, empty(builder)).await.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn options_asterisk_is_answered_before_any_route_except_over_http3() {
    let app = app(&ALL_LISTENERS);
    for (endpoint, version, status) in [
        (Endpoint::H1, Version::HTTP_11, StatusCode::OK),
        (Endpoint::H2, Version::HTTP_2, StatusCode::OK),
        (Endpoint::Quic, Version::HTTP_3, StatusCode::NOT_FOUND),
    ] {
        let response = send(&app, endpoint, empty(request("OPTIONS", "*").version(version))).await;
        assert_eq!(response.status(), status, "{version:?}");
    }
}

#[tokio::test]
async fn only_a_post_carries_a_body() {
    let app = app(&ALL_LISTENERS);
    let declared = |version, length: &str| {
        empty(
            request("GET", "/nope")
                .version(version)
                .header("content-length", length),
        )
    };
    for (endpoint, version, close) in
        [(Endpoint::H1, Version::HTTP_11, Some("close")), (Endpoint::H2, Version::HTTP_2, None)]
    {
        let response = send(&app, endpoint, declared(version, "5")).await;
        assert_eq!((response.status(), header(&response, "connection")), (StatusCode::BAD_REQUEST, close));
        assert_eq!(text(response).await, "request body not accepted\n");
        assert_eq!(send(&app, endpoint, declared(version, "0")).await.status(), StatusCode::NOT_FOUND);
    }
    let unending = |version| request("GET", "/nope").version(version).body(Unending).unwrap();
    assert_eq!(
        send(&app, Endpoint::H1, unending(Version::HTTP_11)).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(&app, Endpoint::Quic, unending(Version::HTTP_3)).await.status(),
        StatusCode::NOT_FOUND
    );
    let post = request("POST", "/nope")
        .header("content-length", "3")
        .body(Full::new(Bytes::from_static(b"abc")))
        .unwrap();
    assert_eq!(send(&app, Endpoint::H1, post).await.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn every_route_answers_its_cors_preflight_from_the_route_pin() {
    let app = app(&ALL_LISTENERS);
    let pin = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../api/routes.txt")).unwrap();
    let routes = pin
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty());
    for line in routes {
        let [_, path, kind] = line.split('|').map(str::trim).collect::<Vec<_>>()[..] else {
            panic!("{line}")
        };
        let route = Route::from_path(path).unwrap();
        let endpoint = Endpoint::ALL
            .into_iter()
            .find(|endpoint| endpoint.mounts(route))
            .unwrap();
        let response = send(&app, endpoint, empty(request("OPTIONS", path))).await;
        if kind != "http" {
            assert_ne!(response.status(), StatusCode::NO_CONTENT, "{path} is no plain HTTP route");
            continue;
        }
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{path}");
        let cors = [
            ("access-control-allow-origin", "*"),
            ("timing-allow-origin", "*"),
            ("access-control-expose-headers", "X-Graphite-Upload-Refusal, Retry-After"),
            ("access-control-allow-methods", "GET, POST, DELETE, OPTIONS"),
            ("access-control-allow-headers", "*"),
            ("access-control-max-age", "7200"),
        ];
        for (name, value) in cors {
            assert_eq!(header(&response, name), Some(value), "{path} {name}");
        }
        assert_eq!(header(&response, "access-control-allow-credentials"), None);
    }
}

#[test]
fn signed_in_access_echoes_the_origin_and_names_its_credentials() {
    let origin = HeaderValue::from_static("https://ui.example");
    let mut headers = HeaderMap::new();
    Access::Cookie(&origin).apply_measurement(&mut headers);
    let value = |headers: &HeaderMap, name: &str| headers.get(name).map(|value| value.to_str().unwrap().to_owned());
    assert_eq!(value(&headers, "access-control-allow-origin").as_deref(), Some("https://ui.example"));
    assert_eq!(value(&headers, "access-control-allow-credentials").as_deref(), Some("true"));
    let allowed = value(&headers, "access-control-allow-headers");
    assert_eq!(allowed.as_deref(), Some("Authorization, Content-Type, X-CSRF-Token"));
    let exposed = "X-Graphite-Upload-Refusal, Retry-After, Graphite-Meter-Auth, Graphite-Meter-Auth-URL";
    assert_eq!(value(&headers, "access-control-expose-headers").as_deref(), Some(exposed));
    assert_eq!(value(&headers, "vary").as_deref(), Some("Origin"));

    let browser = HeaderValue::from_static("https://app.example");
    Access::Bearer(&browser).apply_measurement(&mut headers);
    assert_eq!(value(&headers, "access-control-allow-origin").as_deref(), Some("https://app.example"));
    assert_eq!(
        value(&headers, "access-control-allow-credentials"),
        None,
        "a bearer grant sends no cookies"
    );
    assert_eq!(
        value(&headers, "access-control-allow-headers").as_deref(),
        Some("Authorization, Content-Type")
    );
    let exposed = format!("{exposed}, Graphite-Meter-Browser-Auth");
    assert_eq!(value(&headers, "access-control-expose-headers"), Some(exposed));
}

#[test]
fn hardening_adds_hsts_only_for_secure_requests() {
    for secure in [false, true] {
        let mut headers = HeaderMap::new();
        harden(&mut headers, secure);
        assert_eq!(headers["referrer-policy"], "same-origin");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["permissions-policy"], "camera=(), microphone=(), geolocation=()");
        assert_eq!(headers.get("strict-transport-security").is_some(), secure);
    }
}

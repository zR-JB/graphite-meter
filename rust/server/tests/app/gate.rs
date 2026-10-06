//! Mounts, request-head rules, methods, CORS and hardening headers.

use super::*;
use graphite_meter_proto::route::Route;
use http::{StatusCode, Version};

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

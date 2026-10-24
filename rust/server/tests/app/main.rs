//! Drives the app as transports do, sending constructed requests with in-memory bodies through `App::handle`.

mod approvals;
mod auth;
mod control;
mod gate;
mod oidc;
mod page;
mod provider;
mod sessions;
mod signin;
mod upload;

use bytes::Bytes;
use graphite_meter_server::{
    app::{App, Connection, Endpoint, Outcome},
    config::{self, Config, Loaded},
    exchange::Exchange,
    transport::body::Body,
};
use http::{Request, Response, request::Builder};
use http_body_util::BodyExt;
use serde_json::Value;
use std::{convert::Infallible, ffi::OsString};
use tokio_util::sync::CancellationToken;

const ALL_LISTENERS: [(&str, &str); 5] = [
    ("GM_TLS_CERT", "/cert.pem"),
    ("GM_TLS_KEY", "/key.pem"),
    ("GM_H1_TLS_ADDR", ":7247"),
    ("GM_H2_ADDR", ":7248"),
    ("GM_H3_ADDR", ":7249"),
];

fn config(env: &[(&str, &str)]) -> Config {
    let lookup = |name: &str| {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    };
    match config::load(lookup, Vec::<OsString>::new(), &mut Vec::new()) {
        Ok(Loaded::Config(config)) => *config,
        other => panic!("{env:?}: {other:?}"),
    }
}

fn app(env: &[(&str, &str)]) -> App {
    App::new(config(env), CancellationToken::new()).unwrap()
}

/// Builds a request naming the host its URI does not.
fn request(method: &str, uri: &str) -> Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "speed.example")
}

fn empty(builder: Builder) -> Request<http_body_util::Full<Bytes>> {
    builder.body(Default::default()).unwrap()
}

fn connection(endpoint: Endpoint, peer: &str) -> Connection {
    Connection::new(endpoint, peer.parse().unwrap())
}

async fn outcome<B: http_body::Body>(
    app: &App,
    endpoint: Endpoint,
    peer: &str,
    request: Request<B>,
) -> Outcome {
    app.handle(request, &connection(endpoint, peer), Exchange::start())
        .await
}

async fn send_from<B: http_body::Body>(
    app: &App,
    endpoint: Endpoint,
    peer: &str,
    request: Request<B>,
) -> Response<Body> {
    match outcome(app, endpoint, peer, request).await {
        Outcome::Response(response)
        | Outcome::WebSocket(response, ..)
        | Outcome::WebTransport(response, ..) => response,
        Outcome::Abort => panic!("the exchange was aborted"),
    }
}

async fn send<B: http_body::Body>(
    app: &App,
    endpoint: Endpoint,
    request: Request<B>,
) -> Response<Body> {
    send_from(app, endpoint, "192.0.2.1", request).await
}

async fn text(response: Response<Body>) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn header<'a>(response: &'a Response<Body>, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .map(|value| value.to_str().unwrap())
}

async fn json(response: Response<Body>) -> Value {
    serde_json::from_str(&text(response).await).unwrap()
}

/// Answers `POST /lease` with `body` from `peer` on the discovery origin.
async fn take_lease(app: &App, peer: &str, builder: Builder, body: Value) -> Response<Body> {
    let request = builder.body(http_body_util::Full::new(Bytes::from(body.to_string())));
    send_from(app, Endpoint::H1, peer, request.unwrap()).await
}

/// A new upload ID, from a lease that ends at once.
async fn mint(app: &App) -> String {
    let body = serde_json::json!({"measurements": 1, "sessions": 0, "uploads": 1});
    let lease = json(take_lease(app, "192.0.2.1", request("POST", "/lease"), body).await).await;
    let token = lease["token"].as_str().unwrap();
    let ended = request("DELETE", "/lease").header("graphite-meter-lease", token);
    send(app, Endpoint::H1, empty(ended)).await;
    lease["uploads"][0].as_str().unwrap().into()
}

async fn active(app: &App) -> u64 {
    let probe = json(send(app, Endpoint::H1, empty(request("GET", "/probe"))).await).await;
    probe["load"]["active"].as_u64().unwrap()
}

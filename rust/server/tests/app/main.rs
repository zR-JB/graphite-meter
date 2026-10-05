//! The app as transports drive it: constructed requests with in-memory bodies through `App::handle`.

mod control;
mod gate;
mod page;
mod upload;

use bytes::Bytes;
use graphite_meter_server::{
    app::{App, Connection, Endpoint, Outcome},
    config::{self, Config, Loaded},
    lane::{Exchange, Work},
    transport::body::Body,
};
use http::{Request, Response, request::Builder};
use http_body_util::BodyExt;
use serde_json::Value;
use std::{
    convert::Infallible,
    ffi::OsString,
    pin::Pin,
    task::{Context, Poll},
};
use tokio_util::sync::CancellationToken;

/// TLS listeners on every native address: each endpoint exists.
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

/// A request naming the host its URI does not.
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
    Connection { endpoint, peer: peer.parse().unwrap(), work: Work::default() }
}

async fn outcome<B: http_body::Body>(app: &App, endpoint: Endpoint, peer: &str, request: Request<B>) -> Outcome {
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
        Outcome::Response(response) | Outcome::WebSocket(response, _) => response,
        Outcome::Abort => panic!("the exchange was aborted"),
    }
}

async fn send<B: http_body::Body>(app: &App, endpoint: Endpoint, request: Request<B>) -> Response<Body> {
    send_from(app, endpoint, "192.0.2.1", request).await
}

async fn text(response: Response<Body>) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn header<'a>(response: &'a Response<Body>, name: &str) -> Option<&'a str> {
    response.headers().get(name).map(|value| value.to_str().unwrap())
}

/// A body of unknown length that never ends.
struct Unending;

impl http_body::Body for Unending {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
        Poll::Pending
    }
}

async fn json(response: Response<Body>) -> Value {
    serde_json::from_str(&text(response).await).unwrap()
}

/// The handlers `/probe` reports in use.
async fn active(app: &App) -> u64 {
    let probe = json(send(app, Endpoint::H1, empty(request("GET", "/probe"))).await).await;
    probe["load"]["active"].as_u64().unwrap()
}

//! The app as transports drive it: constructed requests with in-memory bodies through `App::handle`.

mod gate;

use bytes::Bytes;
use graphite_meter_server::{
    app::{App, Connection, Endpoint, Outcome},
    config::{self, Loaded},
    lane::{Exchange, Work},
    transport::body::Body,
};
use http::{Request, Response, request::Builder};
use http_body_util::BodyExt;
use std::{
    convert::Infallible,
    ffi::OsString,
    pin::Pin,
    task::{Context, Poll},
};

/// TLS listeners on every native address: each endpoint exists.
const ALL_LISTENERS: [(&str, &str); 5] = [
    ("GM_TLS_CERT", "/cert.pem"),
    ("GM_TLS_KEY", "/key.pem"),
    ("GM_H1_TLS_ADDR", ":7247"),
    ("GM_H2_ADDR", ":7248"),
    ("GM_H3_ADDR", ":7249"),
];

fn app(env: &[(&str, &str)]) -> App {
    let lookup = |name: &str| {
        env.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    };
    match config::load(lookup, Vec::<OsString>::new(), &mut Vec::new()) {
        Ok(Loaded::Config(config)) => App::new(*config).unwrap(),
        other => panic!("{env:?}: {other:?}"),
    }
}

/// A request naming the host its URI does not.
fn request(method: &str, uri: &str) -> Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "speed.example")
}

fn connection(endpoint: Endpoint, peer: &str) -> Connection {
    Connection { endpoint, peer: peer.parse().unwrap(), work: Work::default() }
}

async fn outcome<B: http_body::Body>(app: &App, endpoint: Endpoint, request: Request<B>) -> Outcome {
    app.handle(request, &connection(endpoint, "192.0.2.1"), Exchange::start())
        .await
}

async fn send<B: http_body::Body>(app: &App, endpoint: Endpoint, request: Request<B>) -> Response<Body> {
    match outcome(app, endpoint, request).await {
        Outcome::Response(response) => response,
        Outcome::Abort => panic!("the exchange was aborted"),
    }
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

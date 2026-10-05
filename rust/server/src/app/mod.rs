//! HTTP semantics independent of version: the gate, dispatch to control endpoints and engines, and finalizing.

pub mod finalize;
mod gate;
mod response;
pub mod topology;

pub use topology::Endpoint;

use crate::{
    auth::Auth,
    config::{Config, ListenerKind},
    lane::{Exchange, Work},
    transport::body::Body,
};
use gate::Gate;
use graphite_meter_proto::route::Route;
use http::{HeaderValue, Method, Request, Response, StatusCode};
use std::net::IpAddr;

/// The state every request shares; transports hold it in an `Arc`.
pub struct App {
    config: Config,
    auth: Auth,
    /// What a bootstrap probe answer names: the HTTP/3 port.
    alt_svc: Option<HeaderValue>,
}

/// What the accepting transport knows of a request's connection, never anything the request claims.
#[derive(Debug, Clone)]
pub struct Connection {
    pub endpoint: Endpoint,
    /// The socket peer's address.
    pub peer: IpAddr,
    /// The admitted work on the connection.
    pub work: Work,
}

/// What a transport does for a request.
#[derive(Debug)]
pub enum Outcome {
    Response(Response<Body>),
    /// Ends the exchange without an answer: an HTTP/1 connection closes, an HTTP/2 or HTTP/3 stream resets.
    Abort,
}

impl App {
    pub fn new(config: Config) -> Result<Self, String> {
        if config.auth.is_some() {
            return Err("authentication is unavailable in this build".into());
        }
        let alt_svc = config.listener(ListenerKind::H3).and_then(|listener| {
            let port: u16 = listener.address.rsplit_once(':')?.1.parse().ok()?;
            HeaderValue::from_str(&format!("h3=\":{port}\"")).ok()
        });
        Ok(Self { config, auth: Auth::Off, alt_svc })
    }

    /// Answers a request that `exchange` bounds until it is admitted.
    pub async fn handle<B: http_body::Body>(
        &self,
        request: Request<B>,
        connection: &Connection,
        exchange: Exchange,
    ) -> Outcome {
        let version = request.version();
        let route = match self.gate(&request, connection) {
            Gate::Pass { route } => route,
            Gate::Answer(mut answer) => {
                self.finalize(&mut answer, None, false, version);
                return Outcome::Response(answer);
            }
        };
        let probe = route == Some(Route::Probe) && request.method() != Method::OPTIONS;
        let mut outcome = self.dispatch(request, route, connection, exchange).await;
        if let Outcome::Response(response) = &mut outcome {
            self.finalize(response, route, probe && connection.endpoint.bootstrap(), version);
        }
        outcome
    }

    async fn dispatch<B: http_body::Body>(
        &self,
        request: Request<B>,
        route: Option<Route>,
        _connection: &Connection,
        _exchange: Exchange,
    ) -> Outcome {
        let response = match route {
            Some(_) if request.method() == Method::OPTIONS => response::empty(StatusCode::NO_CONTENT),
            _ => response::status(StatusCode::NOT_FOUND),
        };
        Outcome::Response(response)
    }
}

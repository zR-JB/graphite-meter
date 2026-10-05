//! Connections over HTTP/1.1, HTTP/2 and HTTP/3, and the answers they read.
use super::{Client, fault::Fault, quic::Quic};
use bytes::Bytes;
use graphite_meter_http3 as http3;
use graphite_meter_net::{Alpn, RequestForm};
use graphite_meter_proto::{
    discovery::{MAX_RESPONSE_BYTES, Protocol},
    origin::{Origin, Scheme},
};
use http::{HeaderValue, Version, header};
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use std::{sync::Arc, time::Duration};

/// Receive windows that let one connection carry 5 Gbit/s over a 100 ms path.
const H2_STREAM_WINDOW: u32 = 32 << 20;
const H2_CONNECTION_WINDOW: u32 = 64 << 20;
/// The largest DATA frame accepted, as the server sends them: larger frames cost less CPU per byte.
const H2_FRAME_BYTES: u32 = 64 << 10;
/// An HTTP/2 connection that reads nothing this long is pinged, and closed when the ping goes unanswered.
const H2_KEEP_ALIVE: Duration = Duration::from_secs(30);
const H2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

type Body = Empty<Bytes>;

/// Reads a JSON answer, as `Preflight::decode` and its siblings do.
pub type Decode<T> = fn(&[u8]) -> Result<T, serde_json::Error>;

/// One connection to an origin.
pub enum Conn {
    Http1 { sender: http1::SendRequest<Body>, form: RequestForm },
    Http2(http2::SendRequest<Body>),
    Http3 { requests: http3::client::SendRequest, quic: Arc<Quic> },
}

pub(super) type Answer = http::Response<Incoming>;

/// A request that failed; `again` when its connection failed before answering, so it may go on another.
pub(super) struct Failed {
    pub fault: Fault,
    pub again: bool,
}

impl Conn {
    /// A connection to `origin` over `via`; a negotiated one is HTTP/2 when TLS agrees on it.
    pub(super) async fn dial(client: &Client, origin: &Origin, via: Protocol) -> Result<Self, Fault> {
        let alpn = match via {
            Protocol::Http1 => Alpn::Http1,
            Protocol::Http2 => Alpn::Http2,
            Protocol::Http3 => {
                let (quic, requests) = super::quic::dial(origin, client.0.verify, &client.0.runtimes.next()).await?;
                return Ok(Self::Http3 { requests, quic });
            }
            Protocol::Negotiated => Alpn::Negotiated,
        };
        let connection = client.0.connector.connect(origin, alpn).await?;
        let h2 = connection.form == RequestForm::Origin
            && match connection.alpn.as_deref() {
                Some(alpn) => alpn == b"h2",
                None => origin.scheme == Scheme::Http && via == Protocol::Http2,
            };
        if via == Protocol::Http2 && !h2 {
            return Err(Fault::Malformed("server or proxy does not support HTTP/2".into()));
        }
        let io = TokioIo::new(connection.stream);
        if !h2 {
            let (sender, driver) = http1::handshake(io).await.map_err(|error| hyper_fault(&error))?;
            tokio::spawn(async move { drop(driver.await) });
            return Ok(Self::Http1 { sender, form: connection.form });
        }
        let (sender, driver) = http2::Builder::new(TokioExecutor::new())
            .timer(TokioTimer::new())
            .max_frame_size(H2_FRAME_BYTES)
            .initial_stream_window_size(H2_STREAM_WINDOW)
            .initial_connection_window_size(H2_CONNECTION_WINDOW)
            .keep_alive_interval(H2_KEEP_ALIVE)
            .keep_alive_timeout(H2_KEEP_ALIVE_TIMEOUT)
            .keep_alive_while_idle(true)
            .handshake(io)
            .await
            .map_err(|error| hyper_fault(&error))?;
        tokio::spawn(async move { drop(driver.await) });
        Ok(Self::Http2(sender))
    }

    /// Another handle to a multiplexed connection; none for HTTP/1.1.
    pub(super) fn share(&self) -> Option<Self> {
        match self {
            Self::Http1 { .. } => None,
            Self::Http2(sender) => Some(Self::Http2(sender.clone())),
            Self::Http3 { requests, quic } => Some(Self::Http3 { requests: requests.clone(), quic: quic.clone() }),
        }
    }

    /// Whether the connection may take another request.
    pub(super) fn usable(&self) -> bool {
        match self {
            Self::Http1 { sender, .. } => !sender.is_closed(),
            Self::Http2(sender) => !sender.is_closed(),
            Self::Http3 { requests, quic } => !quic.closed() && !requests.going_away(),
        }
    }

    /// Whether an HTTP/1.1 connection finished its last exchange.
    pub(super) fn idle(&self) -> bool {
        matches!(self, Self::Http1 { sender, .. } if sender.is_ready())
    }

    /// Sends a bodyless request with an absolute URI and reads its answer's head.
    pub(super) async fn send(&mut self, mut head: http::Request<()>) -> Result<Answer, Failed> {
        let response = match self {
            Self::Http1 { sender, form } => {
                http1_form(&mut head, form);
                sender.ready().await.map_err(hyper_failed)?;
                sender.send_request(head.map(|()| Body::new())).await
            }
            Self::Http2(sender) => {
                sender.ready().await.map_err(hyper_failed)?;
                sender.send_request(head.map(|()| Body::new())).await
            }
            Self::Http3 { requests, quic } => return http3_send(requests, quic, head).await,
        };
        Ok(response
            .map_err(hyper_failed)?
            .map(|body| Incoming(Source::Hyper(body))))
    }
}

/// An HTTP/1.1 head: in origin form with its Host, or in absolute form with a proxy's credentials.
fn http1_form(head: &mut http::Request<()>, form: &RequestForm) {
    let authority = head
        .uri()
        .authority()
        .map(|authority| HeaderValue::from_str(authority.as_str()));
    if let Some(Ok(host)) = authority {
        head.headers_mut().insert(header::HOST, host);
    }
    match form {
        RequestForm::Origin => {
            let path = head.uri().path_and_query().map_or("/", |path| path.as_str());
            *head.uri_mut() = path.parse().unwrap_or_default();
        }
        RequestForm::Absolute { authorization: Some(authorization) } => {
            if let Ok(value) = HeaderValue::from_str(authorization) {
                head.headers_mut().insert(header::PROXY_AUTHORIZATION, value);
            }
        }
        RequestForm::Absolute { authorization: None } => {}
    }
    *head.version_mut() = Version::HTTP_11;
}

async fn http3_send(
    requests: &http3::client::SendRequest,
    quic: &Arc<Quic>,
    head: http::Request<()>,
) -> Result<Answer, Failed> {
    let (mut send, mut recv) = requests.send_request(head).await.map_err(http3_failed)?.split();
    send.finish().await.map_err(http3_failed)?;
    let response = recv.response().await.map_err(http3_failed)?;
    Ok(response.map(|()| Incoming(Source::Http3 { recv: Box::new(recv), _quic: quic.clone() })))
}

/// A response body, holding the connection it reads from.
pub struct Incoming(Source);

enum Source {
    Hyper(hyper::body::Incoming),
    Http3 { recv: Box<http3::RecvHalf>, _quic: Arc<Quic> },
}

impl Incoming {
    /// The next payload, or `None` at the body's end.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, Fault> {
        match &mut self.0 {
            Source::Hyper(body) => loop {
                let Some(frame) = body.frame().await else { return Ok(None) };
                if let Ok(data) = frame.map_err(|error| hyper_fault(&error))?.into_data() {
                    return Ok(Some(data));
                }
            },
            Source::Http3 { recv, .. } => recv.data().await.map_err(http3_fault),
        }
    }

    /// The body, at most 64 KiB of it, read by `decode`.
    pub async fn json<T>(mut self, decode: Decode<T>) -> Result<T, Fault> {
        let oversized = || Fault::Malformed(format!("control response exceeds {MAX_RESPONSE_BYTES} bytes"));
        if let Source::Hyper(body) = &self.0
            && hyper::body::Body::size_hint(body)
                .exact()
                .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(oversized());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = self.chunk().await? {
            if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
                return Err(oversized());
            }
            bytes.extend_from_slice(&chunk);
        }
        decode(&bytes).map_err(|error| Fault::Malformed(error.to_string()))
    }
}

fn hyper_fault(error: &hyper::Error) -> Fault {
    match () {
        _ if error.is_parse() => Fault::Malformed(error.to_string()),
        _ if error.is_timeout() => Fault::TimedOut("HTTP exchange"),
        _ => Fault::Lost(error.to_string()),
    }
}

/// A hyper failure; a stream the peer reset ends alone, so only a connection's own failure goes again.
fn hyper_failed(error: hyper::Error) -> Failed {
    let source = std::error::Error::source(&error).and_then(|source| source.downcast_ref::<h2::Error>());
    let again = !error.is_user() && !source.is_some_and(h2::Error::is_reset);
    Failed { fault: hyper_fault(&error), again }
}

fn http3_fault(error: http3::Error) -> Fault {
    match error {
        http3::Error::Protocol(_) | http3::Error::Connection { local: true, .. } => Fault::Malformed(error.to_string()),
        http3::Error::TimedOut => Fault::TimedOut("HTTP/3 exchange"),
        error => Fault::Lost(error.to_string()),
    }
}

fn http3_failed(error: http3::Error) -> Failed {
    let again = matches!(
        error,
        http3::Error::Transport(_) | http3::Error::Connection { local: false, .. } | http3::Error::GoingAway
    );
    Failed { fault: http3_fault(error), again }
}

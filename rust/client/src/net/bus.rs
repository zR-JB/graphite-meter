//! The latency bus: a WebSocket, or a WebTransport session's datagrams, one probe or reply a message.
use super::{
    CONTROL_TIMEOUT, Client, Request,
    conn::Session,
    conn::{Conn, Payload, ReadBuffer},
    fault::Fault,
};
use futures_util::{SinkExt, StreamExt};
use graphite_meter_proto::{
    bus::{Ping, Pong},
    discovery::{LatencyTransport, Protocol},
    lane::LaneEnding,
    origin::Origin,
    route::Route,
};
use http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use std::time::Instant;
use tokio::{runtime::Handle, time::timeout};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::{client::generate_key, derive_accept_key},
        protocol::{CloseFrame, Role, WebSocketConfig},
    },
};

type Socket = WebSocketStream<TokioIo<Upgraded>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatencyPath {
    pub origin: Origin,
    pub transport: LatencyTransport,
}

pub struct Bus(Channel);

enum Channel {
    WebSocket {
        socket: Box<Socket>,
        client: Client,
        origin: Origin,
    },
    Datagrams(Session),
}

impl Client {
    pub async fn bus(&self, path: &LatencyPath) -> Result<Bus, Fault> {
        let channel = match path.transport {
            LatencyTransport::WebTransport => {
                // The session's drivers run beside the prober, never on a pinned runtime that also carries lanes.
                let home = Handle::current();
                Channel::Datagrams(
                    self.session(Some(&home), &path.origin, Route::WtPing, vec![])
                        .await?,
                )
            }
            LatencyTransport::WebSocket => {
                let socket = timeout(CONTROL_TIMEOUT, self.websocket(&path.origin)).await;
                let socket = socket.unwrap_or(Err(Fault::TimedOut("WebSocket upgrade")))?;
                Channel::WebSocket {
                    socket: Box::new(socket),
                    client: self.clone(),
                    origin: path.origin.clone(),
                }
            }
        };
        Ok(Bus(channel))
    }

    /// A WebSocket upgraded from an HTTP/1.1 connection of its own, carrying the grant as requests do.
    async fn websocket(&self, origin: &Origin) -> Result<Socket, Fault> {
        let request = Request::new(Method::GET, origin, Route::Ping);
        let (mut head, key) = (self.head(&request)?, generate_key());
        for (name, value) in [
            (header::CONNECTION, "Upgrade"),
            (header::UPGRADE, "websocket"),
            (header::SEC_WEBSOCKET_VERSION, "13"),
        ] {
            head.headers_mut()
                .insert(name, HeaderValue::from_static(value));
        }
        let key_value =
            HeaderValue::from_str(&key).map_err(|error| Fault::Malformed(error.to_string()))?;
        head.headers_mut()
            .insert(header::SEC_WEBSOCKET_KEY, key_value);
        let mut conn =
            Conn::dial(self, origin, Protocol::Http1, ReadBuffer::Adaptive, None).await?;
        let answer = conn
            .send(head, Payload::empty())
            .await
            .map_err(|failed| failed.fault)?;
        if answer.status() != StatusCode::SWITCHING_PROTOCOLS {
            let refusal = self.refusal(&request, answer.status(), answer.headers());
            return Err(refusal
                .unwrap_or_else(|| Fault::Malformed("WebSocket upgrade was not accepted".into())));
        }
        if !upgraded(answer.headers(), &derive_accept_key(key.as_bytes())) {
            return Err(Fault::Malformed(
                "invalid WebSocket upgrade response".into(),
            ));
        }
        let upgraded = hyper::upgrade::on(answer).await;
        let upgraded = upgraded.map_err(|_| Fault::Lost)?;
        let config = WebSocketConfig::default()
            .read_buffer_size(4096)
            .write_buffer_size(0)
            .max_write_buffer_size(4096)
            .max_message_size(Some(32 << 10))
            .max_frame_size(Some(32 << 10));
        Ok(
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, Some(config))
                .await,
        )
    }
}

/// Whether a 101 answer accepts the upgrade with `accept` and neither a subprotocol nor extensions.
fn upgraded(headers: &HeaderMap, accept: &str) -> bool {
    let header = |name| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
    };
    let mut connection = header(header::CONNECTION).split(',');
    header(header::UPGRADE).eq_ignore_ascii_case("websocket")
        && connection.any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        && header(header::SEC_WEBSOCKET_ACCEPT) == accept
        && !headers.contains_key(header::SEC_WEBSOCKET_PROTOCOL)
        && !headers.contains_key(header::SEC_WEBSOCKET_EXTENSIONS)
}

impl Bus {
    pub async fn send(&mut self, ping: Ping) -> Result<(), Fault> {
        match &mut self.0 {
            Channel::WebSocket { socket, .. } => {
                let sent = socket.send(Message::text(ping.encode())).await;
                sent.map_err(|_| Fault::Lost)
            }
            Channel::Datagrams(session) => {
                session
                    .send_datagram(ping.write(&mut [0; Ping::MAX_BYTES]))
                    .await
            }
        }
    }

    /// The next reply and when its message arrived, skipping any other message; cancel-safe.
    pub async fn next(&mut self) -> Result<(Pong, Instant), Fault> {
        loop {
            let (pong, arrived) = match &mut self.0 {
                Channel::WebSocket {
                    socket,
                    client,
                    origin,
                } => {
                    let message = socket.next().await;
                    let arrived = Instant::now();
                    let pong = match message {
                        Some(Ok(Message::Text(text))) => Pong::decode(text.as_bytes()),
                        Some(Ok(Message::Binary(bytes))) => Pong::decode(&bytes),
                        Some(Ok(Message::Close(frame))) => {
                            return Err(closed(client, origin, frame));
                        }
                        Some(Ok(_)) => None,
                        Some(Err(_)) => return Err(Fault::Lost),
                        None => return Err(Fault::Lost),
                    };
                    (pong, arrived)
                }
                Channel::Datagrams(session) => {
                    let datagram = session.read_datagram().await?;
                    let arrived = Instant::now();
                    (Pong::decode(&datagram), arrived)
                }
            };
            if let Some(pong) = pong {
                return Ok((pong, arrived));
            }
        }
    }
}

/// The fault a close frame names: its lane ending, else a lost bus; a proxy's own 1001 says no shutdown.
fn closed(client: &Client, origin: &Origin, frame: Option<CloseFrame>) -> Fault {
    let ending = frame.and_then(|frame| {
        LaneEnding::find_by(LaneEnding::websocket_code, frame.code.into())
            .filter(|ending| *ending != LaneEnding::Shutdown || frame.reason == ending.reason())
    });
    match ending {
        Some(ending) => client.ending(origin, ending),
        None => Fault::Lost,
    }
}

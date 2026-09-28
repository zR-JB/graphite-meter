//! Bounded WebSocket ping sessions. The listener owns upgrades and admission.

use crate::timeouts::{IDLE_BOUND, WS_CLOSE};
use futures_util::{SinkExt, StreamExt};
use http::{Request, Response, StatusCode, header};
use std::future::Future;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Error, Message,
        error::ProtocolError,
        handshake::server::create_response_with_body,
        protocol::{CloseFrame, Role, WebSocketConfig, frame::coding::CloseCode},
    },
};

pub use graphite_meter_core::failure::LaneEnding as CloseReason;

/// Go's wsPingReadLimit: a valid PING is at most 15 bytes.
const PING_MESSAGE_BYTES: usize = 2048;

/// Authorization must run before this protocol handshake. In public mode the
/// caller supplies no origin restriction; authenticated browser sessions carry
/// the exact approved origin, rather than a hostname wildcard.
pub fn handshake<B>(request: &Request<B>, allowed_origin: Option<&str>) -> Response<()> {
    if let Some(allowed) = allowed_origin
        && let Some(origin) = request.headers().get(header::ORIGIN)
        && !origin.is_empty()
        && origin.as_bytes() != allowed.as_bytes()
    {
        return refusal(StatusCode::FORBIDDEN);
    }
    if request.headers().get_all(header::SEC_WEBSOCKET_KEY).iter().count() > 1 {
        return refusal(StatusCode::BAD_REQUEST);
    }
    // Go accepts token lists spread across repeated upgrade headers. Normalize
    // those lists for Tungstenite, which expects a single Upgrade value.
    let mut normalized = Request::new(());
    *normalized.method_mut() = request.method().clone();
    *normalized.version_mut() = request.version();
    *normalized.headers_mut() = request.headers().clone();
    for (name, token) in [(header::CONNECTION, "Upgrade"), (header::UPGRADE, "websocket")] {
        if request.headers().get_all(&name).iter().any(|value| {
            value
                .to_str()
                .is_ok_and(|value| value.split(',').any(|part| part.trim().eq_ignore_ascii_case(token)))
        }) {
            normalized
                .headers_mut()
                .insert(name, http::HeaderValue::from_static(token));
        }
    }
    match create_response_with_body(&normalized, || ()) {
        Ok(response) => response,
        Err(Error::Protocol(ProtocolError::WrongHttpMethod)) => {
            let mut response = refusal(StatusCode::METHOD_NOT_ALLOWED);
            response
                .headers_mut()
                .insert(header::ALLOW, http::HeaderValue::from_static("GET"));
            response
        }
        Err(Error::Protocol(
            ProtocolError::WrongHttpVersion
            | ProtocolError::MissingConnectionUpgradeHeader
            | ProtocolError::MissingUpgradeWebSocketHeader,
        )) => {
            let mut response = refusal(StatusCode::UPGRADE_REQUIRED);
            response
                .headers_mut()
                .insert(header::CONNECTION, "Upgrade".parse().unwrap());
            response
                .headers_mut()
                .insert(header::UPGRADE, "websocket".parse().unwrap());
            response
        }
        Err(Error::Protocol(ProtocolError::MissingSecWebSocketVersionHeader)) => {
            let mut response = refusal(StatusCode::BAD_REQUEST);
            response
                .headers_mut()
                .insert(header::SEC_WEBSOCKET_VERSION, "13".parse().unwrap());
            response
        }
        Err(_) => refusal(StatusCode::BAD_REQUEST),
    }
}

fn refusal(status: StatusCode) -> Response<()> {
    let mut response = Response::new(());
    *response.status_mut() = status;
    response
}

/// `stream` must already have completed an authorized HTTP upgrade. The caller
/// keeps its capacity permits until this future returns. Cancellation covers
/// both receiving and sending, including a peer that stops reading replies.
pub async fn serve_ping<S>(stream: S, deadline: tokio::time::Instant, stopped: impl Future<Output = CloseReason>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let config = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(8192)
        .max_message_size(Some(PING_MESSAGE_BYTES))
        .max_frame_size(Some(PING_MESSAGE_BYTES));
    let mut socket = WebSocketStream::from_raw_socket(stream, Role::Server, Some(config)).await;
    let close = tokio::select! {
        biased;
        reason = stopped => (CloseCode::from(reason.websocket_code()), reason.reason()),
        result = exchange(&mut socket, deadline) => match result {
            Ok(reason) => (CloseCode::from(reason.websocket_code()), reason.reason()),
            Err(Error::ConnectionClosed | Error::AlreadyClosed) => return,
            Err(Error::Capacity(_)) => (CloseCode::Size, "message too big"),
            Err(Error::Utf8(_)) => (CloseCode::Invalid, "invalid text"),
            Err(Error::Protocol(_)) => (CloseCode::Protocol, "protocol error"),
            Err(_) => return,
        },
    };
    // Close frames must not let an unresponsive peer retain capacity forever.
    let _ = tokio::time::timeout(
        WS_CLOSE,
        socket.close(Some(CloseFrame {
            code: close.0,
            reason: close.1.into(),
        })),
    )
    .await;
}

async fn exchange<S>(socket: &mut WebSocketStream<S>, deadline: tokio::time::Instant) -> Result<CloseReason, Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut idle = tokio::time::Instant::now() + IDLE_BOUND;
    loop {
        let message = match tokio::time::timeout_at(idle.min(deadline), socket.next()).await {
            Ok(Some(message)) => message,
            Ok(None) => return Ok(CloseReason::Finished),
            Err(_) => {
                return Ok(if idle <= deadline {
                    CloseReason::Idle
                } else {
                    CloseReason::Lifetime
                });
            }
        };
        match message? {
            message @ (Message::Text(_) | Message::Binary(_)) => {
                idle = tokio::time::Instant::now() + IDLE_BOUND;
                if let Some(reply) = crate::ping::reply(&message.into_data()) {
                    match tokio::time::timeout_at(idle.min(deadline), socket.send(Message::Text(reply.into()))).await {
                        Ok(result) => result?,
                        Err(_) => {
                            return Ok(if idle <= deadline {
                                CloseReason::Idle
                            } else {
                                CloseReason::Lifetime
                            });
                        }
                    }
                }
            }
            Message::Ping(_) => socket.flush().await?,
            Message::Close(_) => {
                socket.flush().await?;
                return Ok(CloseReason::Finished);
            }
            Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

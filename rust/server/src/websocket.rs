//! Bounded WebSocket ping sessions. The listener owns upgrades and admission.

use futures_util::{SinkExt, StreamExt};
use http::{Request, Response, StatusCode, header};
use std::{future::Future, time::Duration};
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

#[derive(Clone, Copy, Debug)]
pub enum CloseReason {
    Finished,
    AuthenticationRequired,
}

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
    if request
        .headers()
        .get_all(header::SEC_WEBSOCKET_KEY)
        .iter()
        .count()
        > 1
    {
        return refusal(StatusCode::BAD_REQUEST);
    }
    // Go accepts token lists spread across repeated upgrade headers. Normalize
    // those lists for Tungstenite, which expects a single Upgrade value.
    let mut normalized = Request::new(());
    *normalized.method_mut() = request.method().clone();
    *normalized.version_mut() = request.version();
    *normalized.headers_mut() = request.headers().clone();
    for (name, token) in [
        (header::CONNECTION, "Upgrade"),
        (header::UPGRADE, "websocket"),
    ] {
        if request.headers().get_all(&name).iter().any(|value| {
            value.to_str().is_ok_and(|value| {
                value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case(token))
            })
        }) {
            normalized
                .headers_mut()
                .insert(name, http::HeaderValue::from_static(token));
        }
    }
    match create_response_with_body(&normalized, || ()) {
        Ok(response) => response,
        Err(Error::Protocol(ProtocolError::WrongHttpMethod)) => {
            refusal(StatusCode::METHOD_NOT_ALLOWED)
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
pub async fn serve_ping<S>(stream: S, stopped: impl Future<Output = CloseReason>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let config = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(8192)
        .max_message_size(Some(2048))
        .max_frame_size(Some(2048));
    let mut socket = WebSocketStream::from_raw_socket(stream, Role::Server, Some(config)).await;
    let close = tokio::select! {
        biased;
        reason = stopped => match reason {
            CloseReason::Finished => (CloseCode::Normal, ""),
            CloseReason::AuthenticationRequired => (CloseCode::Policy, "authentication required"),
        },
        result = exchange(&mut socket) => match result {
            Ok(()) | Err(Error::ConnectionClosed | Error::AlreadyClosed) => return,
            Err(Error::Capacity(_)) => (CloseCode::Size, "message too big"),
            Err(Error::Utf8(_)) => (CloseCode::Invalid, "invalid text"),
            Err(Error::Protocol(_)) => (CloseCode::Protocol, "protocol error"),
            Err(_) => return,
        },
    };
    // Close frames must not let an unresponsive peer retain capacity forever.
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        socket.close(Some(CloseFrame {
            code: close.0,
            reason: close.1.into(),
        })),
    )
    .await;
}

async fn exchange<S>(socket: &mut WebSocketStream<S>) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(message) = socket.next().await {
        match message? {
            message @ (Message::Text(_) | Message::Binary(_)) => {
                if let Some(reply) = crate::ping::reply(&message.into_data()) {
                    socket.send(Message::Text(reply.into())).await?;
                }
            }
            Message::Ping(_) => socket.flush().await?,
            Message::Close(_) => {
                socket.flush().await?;
                return Ok(());
            }
            Message::Pong(_) | Message::Frame(_) => {}
        }
    }
    Ok(())
}

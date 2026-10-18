//! Runs the WebSocket bus, which answers the upgrade handshake and then PINGs over a lane until it ends.

use super::body::Body;
use crate::{engine::reflect, lane::Lane};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use graphite_meter_proto::lane::LaneEnding;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use std::time::Duration;
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

/// A bus message holds at most this many bytes. A valid PING has at most 15.
const MAX_MESSAGE_BYTES: usize = 2048;
/// The close handshake has this long, so that an unresponsive peer cannot hold its admission.
const CLOSE_BOUND: Duration = Duration::from_secs(5);

/// Accepts an upgrade with a `101` or refuses it, checking a missing upgrade before the method and that before the key.
pub fn handshake<B>(request: &Request<B>) -> Result<Response<Body>, Response<Body>> {
    // Token lists may spread over repeated headers, which tungstenite expects as one value each.
    let mut normalized = Request::new(());
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
                .insert(name, HeaderValue::from_static(token));
        }
    }
    let answer = match create_response_with_body(&normalized, || ()) {
        Err(Error::Protocol(
            ProtocolError::WrongHttpVersion
            | ProtocolError::MissingConnectionUpgradeHeader
            | ProtocolError::MissingUpgradeWebSocketHeader,
        )) => Err(refusal(
            StatusCode::UPGRADE_REQUIRED,
            &[
                (header::CONNECTION, "Upgrade"),
                (header::UPGRADE, "websocket"),
            ],
        )),
        _ if request.method() != Method::GET => Err(refusal(
            StatusCode::METHOD_NOT_ALLOWED,
            &[(header::ALLOW, "GET")],
        )),
        Err(Error::Protocol(ProtocolError::MissingSecWebSocketVersionHeader)) => Err(refusal(
            StatusCode::BAD_REQUEST,
            &[(header::SEC_WEBSOCKET_VERSION, "13")],
        )),
        _ if request
            .headers()
            .get_all(header::SEC_WEBSOCKET_KEY)
            .iter()
            .count()
            > 1 =>
        {
            Err(refusal(StatusCode::BAD_REQUEST, &[]))
        }
        Ok(accepted) => Ok(accepted),
        Err(_) => Err(refusal(StatusCode::BAD_REQUEST, &[])),
    };
    let empty = |answer: Response<()>| answer.map(|()| Body::empty());
    answer.map(empty).map_err(empty)
}

fn refusal(status: StatusCode, headers: &[(header::HeaderName, &'static str)]) -> Response<()> {
    let mut response = Response::new(());
    *response.status_mut() = status;
    for (name, value) in headers {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}

/// Runs the bus over an upgraded `stream`, after the bytes the handshake `read` past its head.
pub async fn serve<S: AsyncRead + AsyncWrite + Unpin>(stream: S, read: Bytes, lane: Lane) {
    let config = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(8192)
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let mut socket =
        WebSocketStream::from_partially_read(stream, read.into(), Role::Server, Some(config)).await;
    let (code, reason) = tokio::select! {
        biased;
        ending = lane.ended() => close(ending),
        result = exchange(&mut socket, &lane) => match result {
            Ok(()) => close(lane.finish()),
            Err(Error::Capacity(_)) => (CloseCode::Size, "message too big"),
            Err(Error::Utf8(_)) => (CloseCode::Invalid, "invalid text"),
            Err(Error::Protocol(_)) => (CloseCode::Protocol, "protocol error"),
            Err(_) => return,
        },
    };
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    let closing = async {
        socket.close(Some(frame)).await?;
        // Reading until the peer's close keeps its unread messages from resetting the connection.
        while socket.next().await.transpose()?.is_some() {}
        Ok::<_, Error>(())
    };
    let _ = tokio::time::timeout(CLOSE_BOUND, closing).await;
}

fn close(ending: LaneEnding) -> (CloseCode, &'static str) {
    (CloseCode::from(ending.websocket_code()), ending.reason())
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    lane: &Lane,
) -> Result<(), Error> {
    while let Some(message) = socket.next().await {
        let received = std::time::Instant::now();
        match message? {
            message @ (Message::Text(_) | Message::Binary(_)) => {
                lane.moved();
                if let Some(pong) = reflect(&message.into_data(), received) {
                    socket.send(Message::Text(pong.into())).await?;
                }
            }
            Message::Close(_) => {
                socket.flush().await?;
                break;
            }
            Message::Ping(_) => socket.flush().await?,
            Message::Pong(_) | Message::Frame(_) => {}
        }
    }
    Ok(())
}

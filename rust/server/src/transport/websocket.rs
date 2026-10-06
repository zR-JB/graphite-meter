//! The WebSocket bus: the upgrade handshake, then PINGs answered over a lane until it ends.

use super::body::Body;
use crate::{engine::reflect, lane::Lane};
use futures_util::{SinkExt, StreamExt};
use graphite_meter_proto::lane::LaneEnding;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use hyper::upgrade::{OnUpgrade, Upgraded};
use hyper_util::rt::TokioIo;
use std::time::Duration;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Error, Message,
        error::ProtocolError,
        handshake::server::create_response_with_body,
        protocol::{CloseFrame, Role, WebSocketConfig, frame::coding::CloseCode},
    },
};

/// A bus message holds at most this many bytes; a valid PING has at most 15.
const MAX_MESSAGE_BYTES: usize = 2048;
/// The close handshake, so that an unresponsive peer cannot hold its admission.
const CLOSE_BOUND: Duration = Duration::from_secs(5);

/// The `101` accepting an upgrade or its refusal, in Go's order; HEAD gets `Allow: GET`, HTTP/1.0 `Upgrade: websocket`.
pub fn handshake<B>(request: &Request<B>) -> Response<Body> {
    // Token lists may spread over repeated headers, which tungstenite expects as one value each.
    let mut normalized = Request::new(());
    *normalized.version_mut() = request.version();
    *normalized.headers_mut() = request.headers().clone();
    for (name, token) in [(header::CONNECTION, "Upgrade"), (header::UPGRADE, "websocket")] {
        if request.headers().get_all(&name).iter().any(|value| {
            value
                .to_str()
                .is_ok_and(|value| value.split(',').any(|part| part.trim().eq_ignore_ascii_case(token)))
        }) {
            normalized.headers_mut().insert(name, HeaderValue::from_static(token));
        }
    }
    let answer = match create_response_with_body(&normalized, || ()) {
        Err(Error::Protocol(
            ProtocolError::WrongHttpVersion
            | ProtocolError::MissingConnectionUpgradeHeader
            | ProtocolError::MissingUpgradeWebSocketHeader,
        )) => refusal(
            StatusCode::UPGRADE_REQUIRED,
            &[(header::CONNECTION, "Upgrade"), (header::UPGRADE, "websocket")],
        ),
        _ if request.method() != Method::GET => refusal(StatusCode::METHOD_NOT_ALLOWED, &[(header::ALLOW, "GET")]),
        Err(Error::Protocol(ProtocolError::MissingSecWebSocketVersionHeader)) => {
            refusal(StatusCode::BAD_REQUEST, &[(header::SEC_WEBSOCKET_VERSION, "13")])
        }
        _ if request.headers().get_all(header::SEC_WEBSOCKET_KEY).iter().count() > 1 => {
            refusal(StatusCode::BAD_REQUEST, &[])
        }
        Ok(accepted) => accepted,
        Err(_) => refusal(StatusCode::BAD_REQUEST, &[]),
    };
    answer.map(|()| Body::empty())
}

fn refusal(status: StatusCode, headers: &[(header::HeaderName, &'static str)]) -> Response<()> {
    let mut response = Response::new(());
    *response.status_mut() = status;
    for (name, value) in headers {
        response.headers_mut().insert(name, HeaderValue::from_static(value));
    }
    response
}

/// Runs the bus over the upgraded connection until `lane` ends or the peer leaves.
pub async fn serve(upgrade: OnUpgrade, lane: Lane) {
    let upgraded = tokio::select! {
        biased;
        _ = lane.ended() => return,
        upgraded = upgrade => match upgraded {
            Ok(upgraded) => upgraded,
            Err(_) => return,
        },
    };
    let config = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(8192)
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let mut socket = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config)).await;
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
    let frame = CloseFrame { code, reason: reason.into() };
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

/// Answers each PING, text or binary, until the peer closes.
async fn exchange(socket: &mut WebSocketStream<TokioIo<Upgraded>>, lane: &Lane) -> Result<(), Error> {
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

//! One HTTP/3 connection owner dispatches independently owned WebTransport sessions.
use std::{
    collections::HashMap,
    error::Error,
    future::Future,
    future::poll_fn,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use h3::{frame::FrameStream, stream::BufRecvStream};

pub type TransportError = Box<dyn Error + Send + Sync>;
pub type Request = h3::server::RequestResolver<h3_noq::Connection, Bytes>;
pub type ReceiveStream = BufRecvStream<h3_noq::RecvStream, Bytes>;

pub enum Incoming {
    Request(Box<Request>),
    Unidirectional {
        session_id: u64,
        stream: ReceiveStream,
    },
    Datagram {
        session_id: u64,
        payload: Bytes,
    },
}

/// The connection owner must keep polling this while request/session tasks run.
/// Session tasks never accept directly from the underlying QUIC connection.
pub struct Connection {
    http: h3::server::Connection<h3_noq::Connection, Bytes>,
    quic: quinn::Connection,
    pending_headers: HashMap<u64, PendingHeader>,
}

struct PendingHeader {
    deadline: Pin<Box<tokio::time::Sleep>>,
    seen: bool,
}

impl Connection {
    pub async fn new(quic: quinn::Connection, max_sessions: u64) -> Result<Self, TransportError> {
        let http = h3::server::builder()
            .max_field_section_size(32 * 1024)
            .enable_extended_connect(true)
            .enable_datagram(true)
            .enable_webtransport(true)
            .max_webtransport_sessions(max_sessions)
            .build(h3_noq::Connection::new(quic.clone()))
            .await?;
        Ok(Self {
            http,
            quic,
            pending_headers: HashMap::new(),
        })
    }

    pub fn quic(&self) -> &quinn::Connection {
        &self.quic
    }

    pub async fn next(&mut self) -> Result<Option<Incoming>, TransportError> {
        let http = &mut self.http;
        let pending = &mut self.pending_headers;
        tokio::select! {
            incoming = poll_fn(|cx| {
                let result = poll_stream(http, cx);
                for header in pending.values_mut() { header.seen = false; }
                for id in http.inner.pending_recv_stream_ids() {
                    pending.entry(id.into_inner()).or_insert_with(|| PendingHeader {
                        deadline: Box::pin(tokio::time::sleep(Duration::from_secs(10))),
                        seen: true,
                    }).seen = true;
                }
                pending.retain(|_, header| header.seen);
                for header in pending.values_mut() {
                    if header.deadline.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into()));
                    }
                }
                result.map(|result| result.map_err(Into::into))
            }) => incoming,
            datagram = self.quic.read_datagram() => {
                Ok(Some(decode_datagram(datagram?)?))
            }
        }
    }
}

fn decode_datagram(mut payload: Bytes) -> Result<Incoming, TransportError> {
    let Some((quarter_id, length)) = graphite_meter_core::capsule::decode_varint(&payload)? else {
        return Err("truncated HTTP datagram session identifier".into());
    };
    if quarter_id >= (1 << 60) {
        return Err("HTTP datagram session identifier exceeds stream ID range".into());
    }
    let _ = payload.split_to(length);
    Ok(Incoming::Datagram {
        session_id: quarter_id * 4,
        payload,
    })
}

fn poll_stream(
    connection: &mut h3::server::Connection<h3_noq::Connection, Bytes>,
    cx: &mut Context<'_>,
) -> Poll<Result<Option<Incoming>, h3::error::ConnectionError>> {
    // Drain already classified streams before accepting another request. Otherwise
    // a continuous request backlog can starve uni streams held inside h3.
    if let Some(incoming) = take_uni(connection) {
        return Poll::Ready(Ok(Some(incoming)));
    }
    // This also polls control/QPACK streams and accepts new uni streams. Never
    // await one session's payload before driving the rest of the connection.
    let request = connection.poll_accept_request_stream(cx);
    if let Poll::Ready(result) = request {
        return Poll::Ready(result.map(|stream| {
            stream.map(|stream| {
                Incoming::Request(Box::new(
                    connection.create_resolver(FrameStream::new(BufRecvStream::new(stream))),
                ))
            })
        }));
    }
    if let Some(incoming) = take_uni(connection) {
        return Poll::Ready(Ok(Some(incoming)));
    }
    Poll::Pending
}

fn take_uni(
    connection: &mut h3::server::Connection<h3_noq::Connection, Bytes>,
) -> Option<Incoming> {
    connection
        .inner
        .accepted_streams_mut()
        .wt_uni_streams
        .pop()
        .map(|(id, stream)| {
            let session_id = h3::quic::StreamId::from(id).into_inner();
            Incoming::Unidirectional { session_id, stream }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datagrams_route_by_full_connect_id_without_changing_payload() {
        for quarter_id in [0, 1, 63, 64, 16384, (1 << 60) - 1] {
            let mut encoded = Vec::new();
            graphite_meter_core::capsule::encode_varint(quarter_id, &mut encoded).unwrap();
            encoded.extend_from_slice(b"PING,42");
            let Incoming::Datagram {
                session_id,
                payload,
            } = decode_datagram(encoded.into()).unwrap()
            else {
                panic!("datagram expected");
            };
            assert_eq!(session_id, quarter_id * 4);
            assert_eq!(payload, b"PING,42"[..]);
        }
    }

    #[test]
    fn rejects_truncated_or_unrepresentable_session_ids() {
        for encoded in [
            &b""[..],
            &b"\x40"[..],
            &b"\xd0\x00\x00\x00\x00\x00\x00\x00"[..],
        ] {
            assert!(decode_datagram(Bytes::copy_from_slice(encoded)).is_err());
        }
    }
}

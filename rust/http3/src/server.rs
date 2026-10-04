//! The server role: a connection yields requests, and each resolves its head within 10 s.
use crate::{
    charge::{Budget, Charge},
    code::Code,
    connection::{self, Role},
    error::Error,
    fields,
    qpack::Invalid,
    stream::RequestStream,
};
use http::StatusCode;
use std::{future::poll_fn, sync::Arc, time::Duration};

const HEADER_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Connection(connection::Connection);

impl Connection {
    /// `budget` is the one noq charges this connection's buffers to.
    pub fn new(quic: noq::Connection, budget: Budget) -> Self {
        Self(connection::Connection::new(quic, budget, Role::Server))
    }

    /// The next request; `None` once the connection closed gracefully, idle or drained.
    pub async fn next(&mut self) -> Result<Option<Request>, Error> {
        Ok(poll_fn(|cx| self.0.poll_next(cx)).await?.map(Request))
    }

    /// Sends GOAWAY: later requests are refused, and the connection closes once the others end.
    pub fn goaway(&mut self) {
        self.0.goaway();
    }

    /// Also ends the session with `code` and `reason`, even one accepted later, and closes the
    /// connection once the other requests end or 5 s pass.
    pub fn shutdown(&mut self, code: u32, reason: &str) {
        self.0.shutdown(code, reason);
    }
}

/// A request whose head has not been read.
pub struct Request(RequestStream);

impl Request {
    /// Refuses a request over the application's limits with H3_REQUEST_REJECTED.
    pub fn reject(mut self) {
        self.0.abort(Code::H3_REQUEST_REJECTED);
    }

    /// Reads and checks the head. A head over 4 KiB gets 431, and CONNECT for anything but
    /// WebTransport gets 400; both then fail with `Refused`. One the budget cannot hold is aborted
    /// with H3_REQUEST_REJECTED, which lets the client send it again.
    pub async fn resolve(self) -> Result<(http::Request<()>, RequestStream), Error> {
        let mut stream = self.0;
        let resolved = tokio::time::timeout(HEADER_TIMEOUT, async {
            let refusal = match poll_fn(|cx| stream.recv.poll_head(cx)).await {
                Ok(section) => match fields::decode_request(&section, Role::Server.field_limit()) {
                    Ok(head) => {
                        let bytes = head.size as usize
                            + head.message.headers().len() * size_of::<(http::HeaderName, http::HeaderValue)>();
                        let Some(charge) = Charge::new(&stream.shared().budget, bytes) else {
                            return Err(stream.recv_abort(Code::H3_REQUEST_REJECTED));
                        };
                        let mut request = head.message;
                        if request.method() != http::Method::CONNECT {
                            stream.shared().state().sessions.served = true;
                        }
                        // The decoded head's bytes, refunded when the request's extensions drop.
                        request.extensions_mut().insert(Arc::new(charge));
                        stream.recv.message.content_length(head.content_length);
                        return Ok(request);
                    }
                    Err(Invalid::TooLarge) => StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
                    Err(Invalid::Unsupported) => StatusCode::BAD_REQUEST,
                    Err(Invalid::Malformed) => return Err(stream.recv_abort(Code::H3_MESSAGE_ERROR)),
                    Err(Invalid::Qpack) => return Err(stream.shared().close(Code::QPACK_DECOMPRESSION_FAILED)),
                },
                Err(Error::Protocol(Code::H3_EXCESSIVE_LOAD)) => StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
                Err(Error::Protocol(code)) => {
                    stream.send.reset(code);
                    return Err(Error::Protocol(code));
                }
                Err(error) => return Err(error),
            };
            if refusal == StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE {
                stream.recv.stop(Code::H3_EXCESSIVE_LOAD);
            }
            let mut response = http::Response::new(());
            *response.status_mut() = refusal;
            stream.send.send_response(response).await?;
            stream.send.finish().await?;
            Err(Error::Refused)
        })
        .await;
        match resolved {
            Ok(Ok(request)) => Ok((request, stream)),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                stream.abort(Code::H3_REQUEST_INCOMPLETE);
                Err(Error::TimedOut)
            }
        }
    }
}

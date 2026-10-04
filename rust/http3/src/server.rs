//! The server role: a connection yields requests, and each resolves its head within 10 s.
use crate::{
    budget::{Budget, Charge},
    code::Code,
    driver::{Driver, Role},
    error::Error,
    fields::{self, Invalid},
    stream::RequestStream,
};
use http::StatusCode;
use std::{future::poll_fn, sync::Arc, time::Duration};

const HEADER_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Connection(Driver);

impl Connection {
    /// `budget` is the one noq charges this connection's buffers to.
    pub fn new(quic: noq::Connection, budget: Budget) -> Self {
        Self(Driver::new(quic, budget, Role::Server))
    }

    /// The next request; `None` once the connection closed gracefully, idle or drained. Cancel safe.
    pub async fn next(&mut self) -> Result<Option<Request>, Error> {
        Ok(self.0.next().await?.map(Request))
    }

    /// Sends GOAWAY: later requests are refused, and the connection closes once the others end.
    pub fn goaway(&mut self) {
        self.0.goaway();
    }

    /// Also ends the session with `code` and `reason`, even one accepted later, and closes the connection
    /// once the other requests end or 5 s pass.
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

    /// Reads and checks the head. A head over 4 KiB gets 431 and CONNECT for anything but WebTransport
    /// gets 400; both then fail with `Refused`. A head the budget cannot hold is refused with
    /// H3_REQUEST_REJECTED, which lets the client send it again.
    pub async fn resolve(self) -> Result<(http::Request<()>, RequestStream), Error> {
        let mut stream = self.0;
        match tokio::time::timeout(HEADER_TIMEOUT, head(&mut stream)).await {
            Ok(Ok(request)) => Ok((request, stream)),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                stream.abort(Code::H3_REQUEST_INCOMPLETE);
                Err(Error::TimedOut)
            }
        }
    }
}

/// Reads the head into a request, or answers its refusal.
async fn head(stream: &mut RequestStream) -> Result<http::Request<()>, Error> {
    let invalid = match poll_fn(|cx| stream.recv.poll_head(cx)).await {
        Ok(section) => match fields::decode_request(&section, stream.shared().field_limit()) {
            Ok(head) => return admit(stream, head),
            Err(invalid) => invalid,
        },
        Err(Error::Protocol(Code::H3_EXCESSIVE_LOAD)) => Invalid::TooLarge,
        Err(Error::Protocol(code)) => {
            stream.send.reset(code);
            return Err(Error::Protocol(code));
        }
        Err(error) => return Err(error),
    };
    let status = match invalid {
        Invalid::TooLarge => {
            stream.recv.stop(Code::H3_EXCESSIVE_LOAD);
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
        }
        Invalid::Unsupported => StatusCode::BAD_REQUEST,
        Invalid::Malformed => return Err(stream.abort(Code::H3_MESSAGE_ERROR)),
        Invalid::Qpack => return Err(stream.shared().close(Code::QPACK_DECOMPRESSION_FAILED)),
    };
    let mut response = http::Response::new(());
    *response.status_mut() = status;
    stream.send.send_response(response).await?;
    stream.send.finish().await?;
    Err(Error::Refused)
}

/// Charges the decoded head until the request's extensions drop.
fn admit(stream: &mut RequestStream, head: fields::Head<http::Request<()>>) -> Result<http::Request<()>, Error> {
    let bytes = head.size as usize + head.message.headers().len() * size_of::<(http::HeaderName, http::HeaderValue)>();
    let Some(charge) = Charge::new(&stream.shared().budget, bytes) else {
        return Err(stream.abort(Code::H3_REQUEST_REJECTED));
    };
    let mut request = head.message;
    if request.method() != http::Method::CONNECT {
        stream.shared().sessions().served = true;
    }
    request.extensions_mut().insert(Arc::new(charge));
    stream.recv.message.content_length(head.content_length);
    Ok(request)
}

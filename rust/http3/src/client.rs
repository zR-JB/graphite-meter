//! The client role: a driver to run, and a handle that sends requests.
use crate::{
    connection::{self, Role, Shared},
    error::Error,
    fields,
    stream::{self, RequestStream},
};
use std::{future::poll_fn, sync::Arc};

pub fn new(quic: noq::Connection) -> (Connection, SendRequest) {
    let connection = connection::Connection::new(quic, None, Role::Client);
    let requests = SendRequest(connection.shared.clone());
    (Connection(connection), requests)
}

/// Must be driven while requests run; dropping it closes the connection.
pub struct Connection(connection::Connection);

impl Connection {
    /// Runs until the connection closes; `Ok` when it closed gracefully.
    pub async fn drive(&mut self) -> Result<(), Error> {
        poll_fn(|cx| self.0.poll_next(cx)).await.map(drop)
    }
}

#[derive(Clone)]
pub struct SendRequest(Arc<Shared>);

impl SendRequest {
    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.0
    }

    /// Opens a request stream and sends the head, within the server's field section limit.
    pub async fn send_request(&self, request: http::Request<()>) -> Result<RequestStream, Error> {
        if self.0.going_away() {
            return Err(Error::GoingAway);
        }
        let (parts, ()) = request.into_parts();
        let head = fields::encode_request(&parts, None, self.0.peer_field_limit()).map_err(|_| Error::Refused)?;
        let charges = stream::charges(&self.0.budget).ok_or(Error::Refused)?;
        let (send, recv) = self.0.quic.open_bi().await?;
        let mut stream = RequestStream::new(&self.0, send, recv, Role::Client.field_limit(), charges);
        stream.recv.method = parts.method;
        stream.send.send_request(head).await?;
        Ok(stream)
    }
}

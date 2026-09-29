//! One explicitly owned WebTransport session per QUIC connection.
use crate::{
    Error,
    net::Http,
    quic::{Connection, Origin},
    transport::{REDIAL_WINDOW, TransferRetry, restore},
};
use bytes::Bytes;
use futures_util::FutureExt;
use graphite_meter_http3::webtransport::{self as layer, RecvStream, SendStream};
use http::Request;
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::Mutex,
    time::{Instant, timeout},
};

pub struct Session {
    connection: Connection,
    session: layer::Session,
}

impl Session {
    /// One session, on a connection of its own, to `target`, an absolute HTTPS URL: its grant
    /// goes to that URL alone, redirects are never followed, and a refusal is the server's answer.
    pub async fn dial(http: &Http, target: &str, deadline: Duration) -> Result<Self, Error> {
        let mut request = Request::get(target).body(())?;
        http.authorize(target, request.headers_mut())?;
        timeout(deadline, async {
            let (connection, requests) = Connection::dial(&Origin::from_uri(request.uri())?, http.insecure).await?;
            match layer::Session::connect(&requests, request).await {
                Ok(Ok((session, _))) => Ok(Self { connection, session }),
                Ok(Err(response)) => {
                    http.check_status(target, response.status(), response.headers())?;
                    Err(format!("WebTransport CONNECT refused: {}", response.status()).into())
                }
                Err(graphite_meter_http3::Error::NoWebTransport) => Err("peer did not negotiate WebTransport".into()),
                Err(error) => Err(error.into()),
            }
        })
        .await?
    }

    fn ended(&self) -> Option<Result<(u32, String), graphite_meter_http3::Error>> {
        self.session.closed().now_or_never()
    }

    /// Closed with its connection, or once the server ended the session.
    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some() || self.ended().is_some()
    }
    pub async fn send_datagram(&self, payload: &[u8]) -> Result<(), Error> {
        Ok(self.session.send_datagram_wait(payload).await?)
    }
    pub async fn recv_datagram(&self) -> Result<Bytes, Error> {
        self.session
            .read_datagram()
            .await
            .ok_or_else(|| "WebTransport session closed".into())
    }
    pub async fn accept_uni(&self) -> Result<RecvStream, Error> {
        self.session
            .accept_uni()
            .await
            .ok_or_else(|| "WebTransport session closed".into())
    }
    pub async fn open_uni(&self) -> Result<SendStream, Error> {
        Ok(self.session.open_uni().await?)
    }
    pub async fn close(self) {
        self.connection.close(b"session complete").await;
    }
}

/// A stage group shares one live session. Replacement is serialized, so a
/// connection loss cannot make every lane dial its own replacement session.
pub struct SessionSlot {
    current: Mutex<Arc<Session>>,
    http: Http,
    target: String,
}

impl SessionSlot {
    pub async fn dial(http: &Http, target: String) -> Result<Self, Error> {
        let session = Self::open(http, &target).await?;
        Ok(Self {
            current: Mutex::new(Arc::new(session)),
            http: http.clone(),
            target,
        })
    }

    /// Go's stage session dial and redial (webtransport.go:104-154), tried again for 2 s.
    async fn open(http: &Http, target: &str) -> Result<Session, Error> {
        let deadline = Instant::now() + REDIAL_WINDOW;
        restore("WebTransport session", deadline, || {
            Session::dial(http, target, REDIAL_WINDOW)
        })
        .await
    }

    pub async fn current(&self) -> Arc<Session> {
        self.current.lock().await.clone()
    }

    async fn reconnect(&self, failed: &Arc<Session>) -> Result<(), Error> {
        let mut current = self.current.lock().await;
        if !Arc::ptr_eq(&current, failed) {
            return Ok(());
        }
        if !failed.is_closed() {
            return Err("WebTransport stream failed while its session remained open".into());
        }
        *current = Arc::new(Self::open(&self.http, &self.target).await?);
        Ok(())
    }

    /// Go's runWTLane: `attempt` on the current session again and again, paced by `retry`, with
    /// the session dialled again once it closed. An attempt says whether it moved bytes.
    pub(crate) async fn lane<F>(
        &self,
        mut retry: TransferRetry,
        mut attempt: impl FnMut(Arc<Session>) -> F,
    ) -> Result<(), Error>
    where
        F: Future<Output = (Result<(), Error>, bool)>,
    {
        loop {
            let session = self.current().await;
            let started = Instant::now();
            let (result, moved) = attempt(session.clone()).await;
            retry.ended(result, started, moved).await?;
            if session.is_closed() {
                let started = Instant::now();
                if let Err(error) = self.reconnect(&session).await {
                    retry.ended(Err(error), started, false).await?;
                }
            }
        }
    }

    pub async fn close(self) {
        let session = self.current.into_inner();
        if let Ok(session) = Arc::try_unwrap(session) {
            session.close().await;
        }
    }
}

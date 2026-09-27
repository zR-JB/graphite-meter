//! One explicitly owned WebTransport session per QUIC connection.
use crate::{
    Error,
    net::Http,
    quic::{self, Connection, Origin},
};
use bytes::Bytes;
use futures_util::FutureExt;
use graphite_meter_http3::webtransport::{self as layer, RecvStream, SendStream};
use http::Request;
use std::{sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::timeout};

pub struct Session {
    connection: Connection,
    session: layer::Session,
}

#[derive(Debug)]
pub struct ConnectRejected {
    pub status: http::StatusCode,
    pub headers: http::HeaderMap,
}
impl std::fmt::Display for ConnectRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WebTransport CONNECT refused: {}", self.status)
    }
}
impl std::error::Error for ConnectRejected {}

impl Session {
    /// Apply the shared pinned-origin grant policy and authentication refusal handling.
    pub async fn dial(
        http: &crate::net::Http,
        target: &str,
        insecure: bool,
        deadline: Duration,
    ) -> Result<Self, Error> {
        let mut request = Request::get(target).body(())?;
        if let Some(auth) = http.authorization(target) {
            if insecure {
                return Err("authenticated operation refuses insecure TLS".into());
            }
            request.headers_mut().insert(http::header::AUTHORIZATION, auth);
        }
        match Self::connect(request, insecure, deadline).await {
            Err(error) => {
                if let Some(rejected) = error.downcast_ref::<ConnectRejected>() {
                    http.check_status(target, rejected.status, &rejected.headers)?;
                }
                Err(error)
            }
            result => result,
        }
    }
    /// Request must be an absolute HTTPS URL. Authorization headers are sent only
    /// to that URL; redirects are never followed. One session owns one connection.
    pub async fn connect(request: Request<()>, insecure: bool, deadline: Duration) -> Result<Self, Error> {
        timeout(deadline, async {
            let (connection, requests) = Connection::dial(&Origin::from_uri(request.uri())?, insecure).await?;
            let session = match layer::Session::connect(&requests, request).await {
                Ok(Ok(session)) => session,
                Ok(Err(response)) => {
                    return Err(Box::new(ConnectRejected {
                        status: response.status(),
                        headers: response.headers().clone(),
                    }) as Error);
                }
                Err(graphite_meter_http3::Error::Refused) => return Err("peer did not negotiate WebTransport".into()),
                Err(error) => return Err(error.into()),
            };
            Ok(Self { connection, session })
        })
        .await?
    }

    /// The upload route's first server stream carries newline-delimited progress.
    pub async fn upload_progress(&self) -> Result<UploadProgress, Error> {
        Ok(UploadProgress {
            stream: self.accept_uni().await?,
            buffered: Vec::new(),
            pending: Bytes::new(),
        })
    }

    fn ended(&self) -> Option<Result<(u32, String), graphite_meter_http3::Error>> {
        self.session.closed().now_or_never()
    }

    /// Closed with its connection, or once the server ended the session.
    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some() || self.ended().is_some()
    }
    pub fn retryable_failure(&self, error: &Error) -> bool {
        match (self.ended(), self.connection.close_reason()) {
            (Some(Ok(_)), _) => true,
            (_, Some(reason)) => quic::retryable(&reason),
            _ => quic::retryable(error.as_ref()),
        }
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
    insecure: bool,
}

impl SessionSlot {
    pub async fn dial(http: &Http, target: String, insecure: bool) -> Result<Self, Error> {
        let session = Session::dial(http, &target, insecure, Duration::from_secs(10)).await?;
        Ok(Self {
            current: Mutex::new(Arc::new(session)),
            http: http.clone(),
            target,
            insecure,
        })
    }

    pub async fn current(&self) -> Arc<Session> {
        self.current.lock().await.clone()
    }

    pub async fn reconnect(&self, failed: &Arc<Session>) -> Result<Arc<Session>, Error> {
        let mut current = self.current.lock().await;
        if !Arc::ptr_eq(&current, failed) {
            return Ok(current.clone());
        }
        if !failed.is_closed() {
            return Err("WebTransport stream failed while its session remained open".into());
        }
        let session = Session::dial(&self.http, &self.target, self.insecure, Duration::from_secs(10)).await?;
        *current = Arc::new(session);
        Ok(current.clone())
    }

    pub async fn close(self) {
        let session = self.current.into_inner();
        if let Ok(session) = Arc::try_unwrap(session) {
            session.close().await;
        }
    }
}

/// Bounded incremental progress decoder; only server-observed counters are returned.
pub struct UploadProgress {
    stream: RecvStream,
    buffered: Vec<u8>,
    pending: Bytes,
}
impl UploadProgress {
    pub async fn next(&mut self) -> Result<graphite_meter_core::wire::UploadProgress, Error> {
        const MAX_LINE: usize = 16 * 1024;
        loop {
            if self.pending.is_empty() {
                self.pending = self.stream.read_chunk().await?.ok_or("upload progress stream closed")?;
            }
            let end = self.pending.iter().position(|&byte| byte == b'\n');
            let count = end.map_or(self.pending.len(), |end| end + 1);
            if self.buffered.len() + count > MAX_LINE {
                return Err("upload progress line exceeds limit".into());
            }
            self.buffered.extend_from_slice(&self.pending.split_to(count));
            if end.is_some() {
                let line = std::mem::take(&mut self.buffered);
                if let Ok(event) = graphite_meter_core::wire::decode_upload_progress(&line) {
                    return Ok(event);
                }
            }
        }
    }
}

//! WebTransport sessions, each on a QUIC connection of its own.
use super::{CONTROL_TIMEOUT, Client, Request, conn::http3_fault, fault::Fault, quic};
use bytes::Bytes;
use futures_util::FutureExt;
use graphite_meter_http3::{
    self as http3,
    webtransport::{self, RecvStream, SendStream},
};
use graphite_meter_proto::{lane::LaneEnding, origin::Origin, route::Route};
use http::Method;
use std::sync::Arc;
use tokio::{runtime::Handle, time::timeout};

/// A WebTransport session with its connection; dropping it closes both.
pub struct Session {
    session: webtransport::Session,
    quic: Arc<quic::Quic>,
    client: Client,
    origin: Origin,
}

impl Client {
    /// A session at `route` on a connection of its own, which `home` runs, else the next pinned runtime.
    pub async fn session(
        &self,
        home: Option<&Handle>,
        origin: &Origin,
        route: Route,
        query: Vec<(&'static str, String)>,
    ) -> Result<Session, Fault> {
        let home = home.cloned().unwrap_or_else(|| self.shared.runtimes.next());
        let request = Request { query, ..Request::new(Method::CONNECT, origin, route) };
        let head = self.head(&request)?;
        let open = async {
            let (quic, requests) = quic::dial(origin, self.shared.verify, &home).await?;
            let connected = webtransport::Session::connect(&requests, head).await;
            match connected.map_err(http3_fault)? {
                Ok((session, _)) => Ok(Session { session, quic, client: self.clone(), origin: origin.clone() }),
                Err(refused) => Err(self
                    .refusal(&request, refused.status(), refused.headers())
                    .unwrap_or_else(|| Fault::Malformed(format!("WebTransport refused with {}", refused.status())))),
            }
        };
        let opened = timeout(CONTROL_TIMEOUT, open).await;
        opened.unwrap_or(Err(Fault::TimedOut("WebTransport session")))
    }
}

impl Session {
    /// Whether the session and its connection still carry streams.
    pub(super) fn usable(&self) -> bool {
        !self.quic.closed() && self.ended().is_none()
    }

    /// The fault the session ended with, once it ended.
    pub(super) fn ended(&self) -> Option<Fault> {
        Some(match self.session.closed().now_or_never()? {
            Ok((code, _)) => match LaneEnding::from_webtransport_code(code) {
                Some(ending) => self.client.ending(&self.origin, ending),
                None => Fault::Lost(format!("WebTransport session closed with code {code}")),
            },
            Err(error) => http3_fault(error),
        })
    }

    /// What `error` on one of the session's streams means: the session's ending once it ended.
    pub fn fault(&self, error: http3::Error) -> Fault {
        self.ended().unwrap_or_else(|| http3_fault(error))
    }

    fn gone(&self) -> Fault {
        self.ended()
            .unwrap_or_else(|| Fault::Lost("WebTransport session ended".into()))
    }

    /// The next stream the server opened.
    pub async fn accept_uni(&self) -> Result<RecvStream, Fault> {
        self.session.accept_uni().await.ok_or_else(|| self.gone())
    }

    pub(super) async fn open_uni(&self) -> Result<SendStream, Fault> {
        self.session.open_uni().await.map_err(|error| self.fault(error))
    }

    pub(super) async fn send_datagram(&self, payload: &[u8]) -> Result<(), Fault> {
        let sent = self.session.send_datagram_wait(payload).await;
        sent.map_err(|error| self.fault(error))
    }

    pub(super) async fn read_datagram(&self) -> Result<Bytes, Fault> {
        self.session.read_datagram().await.ok_or_else(|| self.gone())
    }
}

//! Control connections per origin and protocol: a multiplexed one shared, idle HTTP/1.1 ones reused.
use super::{
    Client, Request,
    conn::{Answer, Conn, Failed, Payload, ReadBuffer},
    fault::Fault,
    lock,
};
use graphite_meter_proto::{
    discovery::Protocol,
    origin::{Origin, Scheme},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{
    runtime::Handle,
    time::{Instant, timeout_at},
};

/// Idle HTTP/1.1 connections kept per origin and protocol.
const IDLE: usize = 32;

#[derive(Default)]
pub(super) struct Connections {
    slots: Mutex<HashMap<(Origin, Protocol), Arc<Slot>>>,
    serial: AtomicU64,
}

#[derive(Default)]
struct Slot {
    /// The multiplexed connection, by its serial.
    shared: Mutex<Option<(u64, Conn)>>,
    /// Held across a dial that may bring the multiplexed connection, which other requests wait for.
    dialing: tokio::sync::Mutex<()>,
    /// HTTP/1.1 connections whose last answer ended.
    idle: Mutex<Vec<Conn>>,
    /// A negotiated dial found HTTP/1.1, so each request without an idle connection dials its own.
    exclusive: AtomicBool,
}

/// A connection taken for one request: the shared one's serial, and whether an earlier request used it.
struct Taken {
    conn: Conn,
    serial: Option<u64>,
    reused: bool,
}

impl Connections {
    /// The answer's head by `deadline`; a request failing unanswered on a reused connection retries once on a new one.
    pub(super) async fn send(
        &self,
        client: &Client,
        via: Protocol,
        request: &Request,
        deadline: Instant,
    ) -> Result<Answer, Fault> {
        let slot = self.slot(&request.origin, via);
        match self.attempt(client, &slot, via, request, deadline, true).await {
            Err((_, true)) => self.attempt(client, &slot, via, request, deadline, false).await,
            result => result,
        }
        .map_err(|(fault, _)| fault)
    }

    /// The multiplexed connection to `origin` over `via`, dialed on the current runtime when there is none.
    pub(super) async fn shared(&self, client: &Client, origin: &Origin, via: Protocol) -> Result<Conn, Fault> {
        let (slot, home) = (self.slot(origin, via), Handle::current());
        Ok(self.take(client, &slot, via, origin, true, Some(&home)).await?.conn)
    }

    /// The runtime the multiplexed QUIC connection to `origin` over `via` runs on, if there is one.
    pub(super) fn home(&self, origin: &Origin, via: Protocol) -> Option<Handle> {
        let slot = self.slot(origin, via);
        let shared = lock(&slot.shared);
        shared.as_ref()?.1.home()
    }

    fn slot(&self, origin: &Origin, via: Protocol) -> Arc<Slot> {
        let mut slots = lock(&self.slots);
        slots.entry((origin.clone(), via)).or_default().clone()
    }

    /// One try; its fault says whether it failed on a reused connection before any answer.
    async fn attempt(
        &self,
        client: &Client,
        slot: &Arc<Slot>,
        via: Protocol,
        request: &Request,
        deadline: Instant,
        reuse: bool,
    ) -> Result<Answer, (Fault, bool)> {
        let head = client.head(request).map_err(|fault| (fault, false))?;
        let taken = timeout_at(deadline, self.take(client, slot, via, &request.origin, reuse, None)).await;
        let taken = taken.map_err(|_| (Fault::TimedOut("control connection"), false));
        let Taken { mut conn, serial, reused } = taken?.map_err(|fault| (fault, false))?;
        match timeout_at(deadline, conn.send(head, Payload::empty())).await {
            Ok(Ok(answer)) => Ok(slot.keep(conn, answer)),
            Ok(Err(Failed { fault, again })) => {
                if again {
                    slot.retire(serial);
                }
                Err((fault, again && reused))
            }
            Err(_) => {
                slot.retire(serial);
                Err((Fault::TimedOut("response headers"), false))
            }
        }
    }

    /// A connection for one request; a QUIC one it dials runs on `home`, else on the next pinned runtime.
    async fn take(
        &self,
        client: &Client,
        slot: &Slot,
        via: Protocol,
        origin: &Origin,
        reuse: bool,
        home: Option<&Handle>,
    ) -> Result<Taken, Fault> {
        if reuse && let Some(taken) = slot.reusable() {
            return Ok(taken);
        }
        let multiplexed = matches!(via, Protocol::Http2 | Protocol::Http3)
            || via == Protocol::Negotiated && origin.scheme == Scheme::Https;
        if !multiplexed || slot.exclusive.load(Ordering::Relaxed) {
            let conn = Conn::dial(client, origin, via, ReadBuffer::Adaptive, home).await?;
            return Ok(Taken { conn, serial: None, reused: false });
        }
        let _dialing = slot.dialing.lock().await;
        if reuse && let Some(taken) = slot.reusable() {
            return Ok(taken);
        }
        let conn = Conn::dial(client, origin, via, ReadBuffer::Adaptive, home).await?;
        let Some(shared) = conn.share() else {
            slot.exclusive.store(true, Ordering::Relaxed);
            return Ok(Taken { conn, serial: None, reused: false });
        };
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        *lock(&slot.shared) = Some((serial, shared));
        Ok(Taken { conn, serial: Some(serial), reused: false })
    }
}

impl Slot {
    /// The usable multiplexed connection, or an idle HTTP/1.1 one.
    fn reusable(&self) -> Option<Taken> {
        let shared = lock(&self.shared);
        if let Some((serial, conn)) = shared.as_ref().filter(|(_, conn)| conn.usable()) {
            return Some(Taken { conn: conn.share()?, serial: Some(*serial), reused: true });
        }
        drop(shared);
        let mut idle = lock(&self.idle);
        idle.retain(Conn::usable);
        let ready = idle.iter().position(Conn::idle)?;
        Some(Taken { conn: idle.swap_remove(ready), serial: None, reused: true })
    }

    /// Keeps an HTTP/1.1 connection for later requests once `answer`'s body ended; one sent mid-body would wait behind.
    fn keep(self: &Arc<Self>, conn: Conn, answer: Answer) -> Answer {
        if conn.share().is_some() {
            return answer;
        }
        let slot = self.clone();
        answer.map(|body| {
            body.then(move || {
                let mut idle = lock(&slot.idle);
                if idle.len() < IDLE {
                    idle.push(conn);
                }
            })
        })
    }

    /// Later requests take another connection than the shared one of `serial`; its open streams go on.
    fn retire(&self, serial: Option<u64>) {
        let mut shared = lock(&self.shared);
        if serial.is_some() && shared.as_ref().map(|(current, _)| *current) == serial {
            *shared = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use graphite_meter_net::Pool;
    use graphite_meter_proto::route::Route;
    use http::Method;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };

    /// A request taking the connection of an answer still being read would wait behind it, as a receiver checkpoint
    /// behind the upload progress feed it shared a connection with.
    #[tokio::test]
    async fn an_http1_connection_rejoins_the_pool_once_its_answer_ended() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (end, ended) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(socket.read_u8().await.unwrap());
            }
            let feed = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n11\r\n{\"type\":\"ready\"}\n\r\n";
            socket.write_all(feed).await.unwrap();
            ended.await.unwrap();
            socket.write_all(b"0\r\n\r\n").await.unwrap();
            let _ = socket.read_u8().await;
        });
        let client = Client::new(false, Arc::new(Pool::inline()));
        let origin = Origin::parse(&format!("http://{address}")).unwrap();
        let feed = Request::new(Method::GET, &origin, Route::UploadProgress);
        let mut answer = client.control(Protocol::Http1, feed).await.unwrap();
        assert!(answer.chunk().await.unwrap().is_some());
        let slot = client.connections.slot(&origin, Protocol::Http1);
        assert!(lock(&slot.idle).is_empty(), "the connection still carries its answer");
        end.send(()).unwrap();
        assert_eq!(answer.chunk().await.unwrap(), None);
        assert_eq!(lock(&slot.idle).len(), 1);
    }
}

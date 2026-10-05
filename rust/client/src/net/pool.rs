//! Control connections per origin and protocol: a multiplexed one shared, idle HTTP/1.1 ones reused.
use super::{
    Client, Request,
    conn::{Answer, Conn, Failed},
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
use tokio::time::{Instant, timeout_at};

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
    /// The answer's head by `deadline`; a request failing on a reused connection before any answer goes once
    /// more on a new one.
    pub(super) async fn send(
        &self,
        client: &Client,
        via: Protocol,
        request: &Request,
        deadline: Instant,
    ) -> Result<Answer, Fault> {
        let slot = {
            let mut slots = lock(&self.slots);
            slots.entry((request.origin.clone(), via)).or_default().clone()
        };
        match self.attempt(client, &slot, via, request, deadline, true).await {
            Err((_, true)) => self.attempt(client, &slot, via, request, deadline, false).await,
            result => result,
        }
        .map_err(|(fault, _)| fault)
    }

    /// One try; its fault says whether it failed on a reused connection before any answer.
    async fn attempt(
        &self,
        client: &Client,
        slot: &Slot,
        via: Protocol,
        request: &Request,
        deadline: Instant,
        reuse: bool,
    ) -> Result<Answer, (Fault, bool)> {
        let head = client.head(request).map_err(|fault| (fault, false))?;
        let taken = timeout_at(deadline, self.take(client, slot, via, &request.origin, reuse)).await;
        let taken = taken.map_err(|_| (Fault::TimedOut("control connection"), false));
        let Taken { mut conn, serial, reused } = taken?.map_err(|fault| (fault, false))?;
        match timeout_at(deadline, conn.send(head)).await {
            Ok(Ok(answer)) => {
                slot.keep(conn);
                Ok(answer)
            }
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

    async fn take(
        &self,
        client: &Client,
        slot: &Slot,
        via: Protocol,
        origin: &Origin,
        reuse: bool,
    ) -> Result<Taken, Fault> {
        if reuse && let Some(taken) = slot.reusable() {
            return Ok(taken);
        }
        let multiplexed = matches!(via, Protocol::Http2 | Protocol::Http3)
            || via == Protocol::Negotiated && origin.scheme == Scheme::Https;
        if !multiplexed || slot.exclusive.load(Ordering::Relaxed) {
            let conn = Conn::dial(client, origin, via).await?;
            return Ok(Taken { conn, serial: None, reused: false });
        }
        let _dialing = slot.dialing.lock().await;
        if reuse && let Some(taken) = slot.reusable() {
            return Ok(taken);
        }
        let conn = Conn::dial(client, origin, via).await?;
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

    /// Keeps an HTTP/1.1 connection for the requests after its answer.
    fn keep(&self, conn: Conn) {
        let mut idle = lock(&self.idle);
        if conn.share().is_none() && idle.len() < IDLE {
            idle.push(conn);
        }
    }

    /// Later requests take another connection than the shared one of `serial`; its open streams go on.
    fn retire(&self, serial: Option<u64>) {
        let mut shared = lock(&self.shared);
        if serial.is_some() && shared.as_ref().map(|(current, _)| *current) == serial {
            *shared = None;
        }
    }
}

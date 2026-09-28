//! Receiver-owned upload totals shared by HTTP and WebTransport lanes.
use crate::client_address::Shares;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_core::{failure::UploadRefusal, wire::UploadProgress};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

pub const MAX_LIVE_UPLOADS: usize = 1000;
pub const MAX_UPLOADS_PER_CLIENT: usize = 32;
const TOKEN_TTL: Duration = Duration::from_secs(120);
// Retain completion and ownership until a signed ID can no longer create state.
pub const UPLOAD_RETENTION: Duration = TOKEN_TTL;

/// Access identity and shared client admission budget are separate values.
/// A browser grant narrows access without creating another subject budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    client_keys: Vec<String>,
}

impl Owner {
    pub fn anonymous(address: std::net::IpAddr) -> Self {
        Self {
            client_keys: crate::client_address::client_keys(address),
        }
    }

    pub fn login(subject: &str, session: &str) -> Self {
        Self {
            client_keys: vec![format!("login:{session}"), format!("principal:{subject}")],
        }
    }

    pub fn principal(subject: impl Into<String>) -> Self {
        Self {
            client_keys: vec![format!("principal:{}", subject.into())],
        }
    }

    pub fn delegated(subject: impl Into<String>, grant_id: impl Into<String>) -> Self {
        let grant_id = grant_id.into();
        Self {
            client_keys: vec![format!("grant:{grant_id}"), format!("principal:{}", subject.into())],
        }
    }

    pub fn client_keys(&self) -> &[String] {
        &self.client_keys
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct UploadCheckpoint {
    pub bytes: u64,
    pub nanos: u64,
}

#[derive(Clone)]
pub struct UploadStore {
    inner: Arc<Store>,
}
struct Store {
    key: [u8; 32],
    origin: Instant,
    next_sweep: Mutex<Instant>,
    entries: Mutex<UploadEntries>,
    meter: crate::meter::Meter,
}
#[derive(Default)]
struct UploadEntries {
    by_id: HashMap<String, Arc<Mutex<Aggregate>>>,
    // Counts retained aggregates, including finished ones, by admission budget.
    by_client: Shares,
    tombstones: HashMap<String, Instant>,
}
struct Aggregate {
    owner: Owner,
    bytes: u64,
    first_chunk: Option<Instant>,
    touched: Instant,
    lanes: usize,
    finished: bool,
    expired: bool,
    claim: Option<Arc<()>>,
    changed: Arc<tokio::sync::Notify>,
}
impl Aggregate {
    fn authorize(&self, owner: &Owner) -> Result<(), UploadRefusal> {
        if &self.owner != owner {
            Err(UploadRefusal::OwnerMismatch)
        } else {
            Ok(())
        }
    }
    fn checkpoint(&self) -> UploadCheckpoint {
        UploadCheckpoint {
            bytes: self.bytes,
            nanos: self.first_chunk.map_or(0, |start| nanos(start.elapsed())),
        }
    }
}
impl UploadStore {
    pub fn new() -> Option<Self> {
        Self::with_meter(crate::meter::Meter::default())
    }

    pub(crate) fn with_meter(meter: crate::meter::Meter) -> Option<Self> {
        let mut key = [0; 32];
        getrandom::fill(&mut key).ok()?;
        Some(Self {
            inner: Arc::new(Store {
                key,
                origin: Instant::now() - Duration::from_nanos(1),
                next_sweep: Mutex::new(Instant::now() + Duration::from_secs(5)),
                entries: Mutex::new(UploadEntries::default()),
                meter,
            }),
        })
    }
    pub(crate) fn log_transfer(&self, window: Duration) {
        self.inner.meter.log("upload", window);
    }
    /// Tokens authenticate themselves; minting consumes no retained aggregate capacity.
    pub fn mint(&self) -> Option<String> {
        let mut raw = [0; 56];
        raw[..8].copy_from_slice(&nanos(self.inner.origin.elapsed()).max(1).to_be_bytes());
        getrandom::fill(&mut raw[8..24]).ok()?;
        let mut mac = token_mac(&self.inner.key);
        mac.update(&raw[..24]);
        let tag = mac.finalize().into_bytes();
        raw[24..].copy_from_slice(&tag);
        Some(format!("gmu_{}", URL_SAFE_NO_PAD.encode(raw)))
    }
    fn valid(&self, id: &str) -> bool {
        let Some(encoded) = id.strip_prefix("gmu_") else {
            return false;
        };
        if encoded.len() != 75 {
            return false;
        }
        let Ok(raw) = URL_SAFE_NO_PAD.decode(encoded) else {
            return false;
        };
        if raw.len() != 56 {
            return false;
        }
        let mut mac = token_mac(&self.inner.key);
        mac.update(&raw[..24]);
        if mac.verify_slice(&raw[24..]).is_err() {
            return false;
        }
        let issued = u64::from_be_bytes(raw[..8].try_into().expect("fixed token timestamp"));
        let now = nanos(self.inner.origin.elapsed());
        issued > 0 && issued <= now && now - issued <= nanos(TOKEN_TTL)
    }
    fn access(
        &self,
        id: &str,
        owner: &Owner,
        create: bool,
        lane: bool,
    ) -> Result<Arc<Mutex<Aggregate>>, UploadRefusal> {
        self.sweep_if_due();
        let mut entries = self.inner.entries.lock().expect("upload store lock");
        let aggregate = if let Some(aggregate) = entries.by_id.get(id) {
            aggregate.clone()
        } else {
            if !create || entries.tombstones.contains_key(id) || !self.valid(id) {
                return Err(UploadRefusal::Invalid);
            }
            if entries.by_client.full(owner.client_keys(), MAX_UPLOADS_PER_CLIENT) {
                return Err(UploadRefusal::ClientFull);
            }
            if entries.by_id.len() >= MAX_LIVE_UPLOADS {
                if entries.tombstones.len() >= MAX_LIVE_UPLOADS {
                    return Err(UploadRefusal::GlobalFull);
                }
                let victim = entries
                    .by_id
                    .iter()
                    .filter_map(|(id, aggregate)| {
                        let state = aggregate.lock().expect("upload aggregate lock");
                        (state.lanes == 0 && state.bytes == 0 && !state.finished).then(|| (id.clone(), state.touched))
                    })
                    .min_by_key(|(_, touched)| *touched);
                let Some((victim, _)) = victim else {
                    return Err(UploadRefusal::GlobalFull);
                };
                let aggregate = entries.by_id.remove(&victim).expect("selected receiver exists");
                let mut state = aggregate.lock().expect("upload aggregate lock");
                entries.by_client.release(state.owner.client_keys());
                state.expired = true;
                state.changed.notify_waiters();
                entries.tombstones.insert(victim, Instant::now() + TOKEN_TTL);
            }
            let aggregate = Arc::new(Mutex::new(Aggregate {
                owner: owner.clone(),
                bytes: 0,
                first_chunk: None,
                touched: Instant::now(),
                lanes: 0,
                finished: false,
                expired: false,
                claim: None,
                changed: Arc::new(tokio::sync::Notify::new()),
            }));
            entries.by_id.insert(id.to_owned(), aggregate.clone());
            entries.by_client.hold(owner.client_keys());
            aggregate
        };
        {
            let mut state = aggregate.lock().expect("upload aggregate lock");
            state.authorize(owner)?;
            // Join under the store lock so sweeping cannot remove a just-admitted lane.
            if lane {
                // Completion is terminal: a late stream must not change a
                // total that a progress subscriber may already have emitted.
                if state.finished {
                    return Err(UploadRefusal::Invalid);
                }
                state.lanes += 1;
                state.touched = Instant::now();
            }
        }
        Ok(aggregate)
    }
    pub fn begin(&self, id: &str, owner: &Owner) -> Result<UploadLane, UploadRefusal> {
        Ok(UploadLane {
            aggregate: self.access(id, owner, true, true)?,
            bytes: 0,
            transfer: self.inner.meter.open(),
        })
    }
    /// Read-only observation never extends retention and never creates an aggregate.
    pub fn checkpoint(&self, id: &str, owner: &Owner) -> Result<UploadCheckpoint, UploadRefusal> {
        Ok(self
            .access(id, owner, false, false)?
            .lock()
            .expect("upload aggregate lock")
            .checkpoint())
    }
    pub fn finish(&self, id: &str, owner: &Owner) -> Result<(), UploadRefusal> {
        let aggregate = self.access(id, owner, false, false)?;
        let mut state = aggregate.lock().expect("upload aggregate lock");
        state.finished = true;
        state.changed.notify_waiters();
        Ok(())
    }
    pub fn subscribe(&self, id: &str, owner: &Owner) -> Result<UploadSubscription, UploadRefusal> {
        let aggregate = self.access(id, owner, true, false)?;
        let claim = Arc::new(());
        let changed = {
            let mut state = aggregate.lock().expect("upload aggregate lock");
            state.claim = Some(claim.clone());
            state.changed.notify_waiters();
            state.changed.clone()
        };
        Ok(UploadSubscription {
            store: self.clone(),
            aggregate,
            claim,
            changed,
            next_tick: Instant::now() + Duration::from_millis(100),
            ready: false,
            ended: false,
        })
    }
    fn sweep_if_due(&self) {
        let now = Instant::now();
        let mut next = self.inner.next_sweep.lock().expect("upload sweep lock");
        if now >= *next {
            self.sweep_at(now);
            *next = now + Duration::from_secs(5);
        }
    }
    /// Also permits a caller-owned maintenance loop; no background task is spawned.
    pub fn sweep_at(&self, now: Instant) {
        let mut entries = self.inner.entries.lock().expect("upload store lock");
        entries.tombstones.retain(|_, until| *until >= now);
        let UploadEntries { by_id, by_client, .. } = &mut *entries;
        by_id.retain(|_, aggregate| {
            let mut state = aggregate.lock().expect("upload aggregate lock");
            if state.lanes == 0 && now.saturating_duration_since(state.touched) > UPLOAD_RETENTION {
                by_client.release(state.owner.client_keys());
                state.expired = true;
                state.changed.notify_waiters();
                false
            } else {
                true
            }
        });
    }
    pub fn retained(&self) -> usize {
        self.inner.entries.lock().expect("upload store lock").by_id.len()
    }
}

/// A transport must record only bytes actually received. Dropping its future releases the lane.
pub struct UploadLane {
    aggregate: Arc<Mutex<Aggregate>>,
    bytes: u64,
    transfer: Option<crate::meter::Transfer>,
}
impl UploadLane {
    /// Datagram lanes have no stream FIN. Observe the explicit finish request
    /// without retaining a borrow of the lane while it records incoming bytes.
    pub fn finished(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let aggregate = self.aggregate.clone();
        async move {
            let changed = aggregate.lock().expect("upload aggregate lock").changed.clone();
            loop {
                let notified = changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                {
                    let state = aggregate.lock().expect("upload aggregate lock");
                    if state.finished || state.expired {
                        return;
                    }
                }
                notified.await;
            }
        }
    }

    pub fn record(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let mut state = self.aggregate.lock().expect("upload aggregate lock");
        let now = Instant::now();
        state.first_chunk.get_or_insert(now);
        state.bytes = state.bytes.saturating_add(bytes as u64);
        self.bytes = self.bytes.saturating_add(bytes as u64);
        if let Some(transfer) = &self.transfer {
            transfer.record(bytes);
        }
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}
impl Drop for UploadLane {
    fn drop(&mut self) {
        let mut state = self.aggregate.lock().expect("upload aggregate lock");
        state.lanes -= 1;
        state.touched = Instant::now();
        state.changed.notify_waiters();
    }
}

/// One current subscriber per aggregate, with immediate lifecycle wakeups and no event queue.
/// Adapters own transport heartbeats and cancellation of the `next` future.
pub struct UploadSubscription {
    store: UploadStore,
    aggregate: Arc<Mutex<Aggregate>>,
    claim: Arc<()>,
    changed: Arc<tokio::sync::Notify>,
    next_tick: Instant,
    ready: bool,
    ended: bool,
}
impl UploadSubscription {
    pub async fn next(&mut self) -> Option<UploadProgress> {
        if self.ended {
            return None;
        }
        let mut ticked = false;
        loop {
            // Register before inspecting state; notify_waiters cannot be lost between
            // the lifecycle check and suspension, including when a prior next was cancelled.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            self.store.sweep_if_due();
            {
                let state = self.aggregate.lock().expect("upload aggregate lock");
                if !state
                    .claim
                    .as_ref()
                    .is_some_and(|claim| Arc::ptr_eq(claim, &self.claim))
                {
                    self.ended = true;
                    return None;
                }
                if state.expired {
                    self.ended = true;
                    return Some(UploadProgress::Error {
                        message: UploadRefusal::Invalid.message().into(),
                        code: UploadRefusal::Invalid.name().into(),
                    });
                }
                if !self.ready {
                    self.ready = true;
                    return Some(UploadProgress::Ready);
                }
                if state.finished && state.lanes == 0 {
                    self.ended = true;
                    let c = state.checkpoint();
                    return Some(UploadProgress::Complete {
                        bytes: c.bytes,
                        nanos: c.nanos,
                    });
                }
                if ticked && !state.finished {
                    let checkpoint = state.checkpoint();
                    if checkpoint.nanos > 0 {
                        return Some(UploadProgress::Progress {
                            bytes: checkpoint.bytes,
                            nanos: checkpoint.nanos,
                        });
                    }
                }
            }
            tokio::select! {
                () = &mut notified => { ticked = false; }
                () = tokio::time::sleep_until(self.next_tick) => {
                    self.next_tick = Instant::now() + Duration::from_millis(100);
                    ticked = true;
                }
            }
        }
    }
}
impl Drop for UploadSubscription {
    fn drop(&mut self) {
        let mut state = self.aggregate.lock().expect("upload aggregate lock");
        if state
            .claim
            .as_ref()
            .is_some_and(|claim| Arc::ptr_eq(claim, &self.claim))
        {
            state.claim = None;
        }
    }
}
fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}
fn token_mac(key: &[u8; 32]) -> Hmac<Sha256> {
    Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 32-byte key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_age_is_checked_only_when_creating_an_aggregate() {
        let mut store = UploadStore::new().unwrap();
        Arc::get_mut(&mut store.inner).unwrap().origin = Instant::now() - Duration::from_secs(300);
        let signed = |issued: u64| {
            let mut raw = [0; 56];
            raw[..8].copy_from_slice(&issued.to_be_bytes());
            let mut mac = token_mac(&store.inner.key);
            mac.update(&raw[..24]);
            raw[24..].copy_from_slice(&mac.finalize().into_bytes());
            format!("gmu_{}", URL_SAFE_NO_PAD.encode(raw))
        };
        assert_eq!(
            store
                .begin(&signed(nanos(Duration::from_secs(1))), &Owner::principal("a"))
                .err(),
            Some(UploadRefusal::Invalid)
        );
        assert_eq!(
            store
                .begin(&signed(nanos(Duration::from_secs(600))), &Owner::principal("a"))
                .err(),
            Some(UploadRefusal::Invalid)
        );
        let id = store.mint().unwrap();
        let mut lane = store.begin(&id, &Owner::principal("a")).unwrap();
        lane.record(15);
        Arc::get_mut(&mut store.inner).unwrap().origin -= Duration::from_secs(121);
        assert!(!store.valid(&id));
        assert_eq!(store.checkpoint(&id, &Owner::principal("a")).unwrap().bytes, 15);
        drop(store.begin(&id, &Owner::principal("a")).unwrap());
    }

    #[test]
    fn observing_and_finishing_do_not_refresh_idle_retention() {
        let store = UploadStore::new().unwrap();
        let id = store.mint().unwrap();
        drop(store.begin(&id, &Owner::principal("a")).unwrap());
        let aggregate = store.inner.entries.lock().unwrap().by_id[&id].clone();
        let old = Instant::now() - Duration::from_secs(80);
        aggregate.lock().unwrap().touched = old;
        store.checkpoint(&id, &Owner::principal("a")).unwrap();
        let _subscription = store.subscribe(&id, &Owner::principal("a")).unwrap();
        store.finish(&id, &Owner::principal("a")).unwrap();
        assert_eq!(aggregate.lock().unwrap().touched, old);
        store.sweep_at(old + UPLOAD_RETENTION + Duration::from_secs(1));
        assert_eq!(store.retained(), 0);
    }

    #[test]
    fn finished_id_cannot_reopen_at_the_last_valid_token_age() {
        let store = UploadStore::new().unwrap();
        let id = store.mint().unwrap();
        let owner = Owner::principal("original");
        drop(store.begin(&id, &owner).unwrap());
        store.finish(&id, &owner).unwrap();
        let aggregate = store.inner.entries.lock().unwrap().by_id[&id].clone();
        let touched = Instant::now() - TOKEN_TTL;
        aggregate.lock().unwrap().touched = touched;

        store.sweep_at(touched + TOKEN_TTL);
        assert_eq!(store.retained(), 1);
        assert_eq!(store.begin(&id, &owner).err(), Some(UploadRefusal::Invalid));
        assert_eq!(
            store.begin(&id, &Owner::principal("other")).err(),
            Some(UploadRefusal::OwnerMismatch)
        );
    }

    #[test]
    fn client_capacity_index_tracks_partial_sweeps() {
        let store = UploadStore::new().unwrap();
        let owner = Owner::principal("a");
        let other = Owner::principal("b");
        let mut first = None;
        for _ in 0..MAX_UPLOADS_PER_CLIENT {
            let id = store.mint().unwrap();
            drop(store.begin(&id, &owner).unwrap());
            first.get_or_insert(id);
        }
        let other_id = store.mint().unwrap();
        drop(store.begin(&other_id, &other).unwrap());
        let first = first.unwrap();
        let aggregate = store.inner.entries.lock().unwrap().by_id[&first].clone();
        aggregate.lock().unwrap().touched = Instant::now() - UPLOAD_RETENTION - Duration::from_secs(1);
        store.sweep_at(Instant::now());

        assert_eq!(store.retained(), MAX_UPLOADS_PER_CLIENT);
        drop(store.begin(&store.mint().unwrap(), &owner).unwrap());
        assert_eq!(
            store.begin(&store.mint().unwrap(), &owner).err(),
            Some(UploadRefusal::ClientFull)
        );
        drop(store.begin(&store.mint().unwrap(), &other).unwrap());
    }
}

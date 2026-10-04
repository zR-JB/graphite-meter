//! Receiver-owned upload totals shared by HTTP and WebTransport lanes.
use crate::{client_address::Shares, sync::lock};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_core::{failure::UploadRefusal, wire::UploadProgress};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::time::Instant;

pub const MAX_LIVE_UPLOADS: usize = 1000;
pub const MAX_UPLOADS_PER_CLIENT: usize = 32;
const TOKEN_TTL: Duration = Duration::from_secs(120);
// Retain completion and ownership until a signed ID can no longer create state.
pub const UPLOAD_RETENTION: Duration = TOKEN_TTL;
/// Go's uploadSweepInterval: expired aggregates are dropped at most this often.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);
/// A subscription reports progress at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Access identity and shared client admission budget are separate values.
/// A browser grant narrows access without creating another subject budget.
/// The owner's client keys, narrowest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner(Vec<String>);

impl Owner {
    pub fn anonymous(address: std::net::IpAddr) -> Self {
        Self(crate::client_address::client_keys(address))
    }

    /// Ambiguous proxy evidence names no client; like Go's empty owner, it can create or reach no upload.
    pub fn unresolved() -> Self {
        Self(Vec::new())
    }

    pub fn login(subject: &str, session: &str) -> Self {
        Self(vec![format!("login:{session}"), Self::principal_key(subject)])
    }

    pub fn delegated(subject: &str, grant_id: &str) -> Self {
        Self(vec![format!("grant:{grant_id}"), Self::principal_key(subject)])
    }

    /// The key every login and grant of `subject` shares.
    pub(crate) fn principal_key(subject: &str) -> String {
        format!("principal:{subject}")
    }

    pub fn client_keys(&self) -> &[String] {
        &self.0
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
    claim: Weak<()>,
    changed: Arc<tokio::sync::Notify>,
}
impl Aggregate {
    fn authorize(&self, owner: &Owner) -> Result<(), UploadRefusal> {
        (&self.owner == owner).then_some(()).ok_or(UploadRefusal::OwnerMismatch)
    }
    fn checkpoint(&self) -> UploadCheckpoint {
        UploadCheckpoint {
            bytes: self.bytes,
            nanos: self.first_chunk.map_or(0, |start| nanos(start.elapsed())),
        }
    }
}
impl UploadStore {
    pub(crate) fn with_meter(meter: crate::meter::Meter) -> Option<Self> {
        let mut key = [0; 32];
        getrandom::fill(&mut key).ok()?;
        Some(Self {
            inner: Arc::new(Store {
                key,
                origin: Instant::now() - Duration::from_nanos(1),
                next_sweep: Mutex::new(Instant::now() + SWEEP_INTERVAL),
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
        let Some(encoded) = id.strip_prefix("gmu_").filter(|encoded| encoded.len() == 75) else {
            return false;
        };
        let mut raw = [0; 56];
        if URL_SAFE_NO_PAD.decode_slice(encoded, &mut raw) != Ok(raw.len()) {
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
        // As in Go, before anything else when joining; a stored owner is never unresolved, so reads mismatch too.
        if create && owner.client_keys().is_empty() {
            return Err(UploadRefusal::OwnerMismatch);
        }
        self.sweep_if_due();
        let mut entries = lock(&self.inner.entries);
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
                        let state = lock(aggregate);
                        (state.lanes == 0 && state.bytes == 0 && !state.finished).then_some((id, state.touched))
                    })
                    .min_by_key(|(_, touched)| *touched)
                    .map(|(id, _)| id.clone());
                let Some(victim) = victim else {
                    return Err(UploadRefusal::GlobalFull);
                };
                let aggregate = entries.by_id.remove(&victim).expect("selected receiver exists");
                let mut state = lock(&aggregate);
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
                claim: Weak::new(),
                changed: Arc::new(tokio::sync::Notify::new()),
            }));
            entries.by_id.insert(id.to_owned(), aggregate.clone());
            entries.by_client.hold(owner.client_keys());
            aggregate
        };
        {
            let mut state = lock(&aggregate);
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
        let aggregate = self.access(id, owner, false, false)?;
        Ok(lock(&aggregate).checkpoint())
    }
    pub fn finish(&self, id: &str, owner: &Owner) -> Result<(), UploadRefusal> {
        let aggregate = self.access(id, owner, false, false)?;
        let mut state = lock(&aggregate);
        state.finished = true;
        state.changed.notify_waiters();
        Ok(())
    }
    pub fn subscribe(&self, id: &str, owner: &Owner) -> Result<UploadSubscription, UploadRefusal> {
        let aggregate = self.access(id, owner, true, false)?;
        let claim = Arc::new(());
        let changed = {
            let mut state = lock(&aggregate);
            state.claim = Arc::downgrade(&claim);
            state.changed.notify_waiters();
            state.changed.clone()
        };
        Ok(UploadSubscription {
            store: self.clone(),
            aggregate,
            claim,
            changed,
            next_tick: Instant::now() + PROGRESS_INTERVAL,
            ready: false,
            ended: false,
        })
    }
    fn sweep_if_due(&self) {
        let now = Instant::now();
        let mut next = lock(&self.inner.next_sweep);
        if now >= *next {
            self.sweep_at(now);
            *next = now + SWEEP_INTERVAL;
        }
    }
    fn sweep_at(&self, now: Instant) {
        let mut entries = lock(&self.inner.entries);
        entries.tombstones.retain(|_, until| *until >= now);
        let UploadEntries { by_id, by_client, .. } = &mut *entries;
        by_id.retain(|_, aggregate| {
            let mut state = lock(aggregate);
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
    pub fn finished(&self) -> impl std::future::Future<Output = ()> + Send + 'static + use<> {
        let aggregate = self.aggregate.clone();
        async move {
            let changed = lock(&aggregate).changed.clone();
            loop {
                let notified = changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                {
                    let state = lock(&aggregate);
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
        let mut state = lock(&self.aggregate);
        state.first_chunk.get_or_insert_with(Instant::now);
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
        let mut state = lock(&self.aggregate);
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
                let state = lock(&self.aggregate);
                // The weak registration keeps its allocation distinct from every newer claim.
                if !std::ptr::eq(state.claim.as_ptr(), Arc::as_ptr(&self.claim)) {
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
                let UploadCheckpoint { bytes, nanos } = state.checkpoint();
                if state.finished && state.lanes == 0 {
                    self.ended = true;
                    return Some(UploadProgress::Complete { bytes, nanos });
                }
                if ticked && !state.finished && nanos > 0 {
                    return Some(UploadProgress::Progress { bytes, nanos });
                }
            }
            tokio::select! {
                () = &mut notified => { ticked = false; }
                () = tokio::time::sleep_until(self.next_tick) => {
                    self.next_tick = Instant::now() + PROGRESS_INTERVAL;
                    ticked = true;
                }
            }
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
#[path = "upload_tests.rs"]
mod tests;

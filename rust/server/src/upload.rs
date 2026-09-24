//! Receiver-owned upload totals shared by HTTP and WebTransport lanes.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_core::wire::UploadProgress;
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const MAX_LIVE_UPLOADS: usize = 1000;
pub const MAX_UPLOADS_PER_CLIENT: usize = 32;
const TOKEN_TTL: Duration = Duration::from_secs(120);
// Retain completion and ownership until a signed ID can no longer create state.
pub const UPLOAD_RETENTION: Duration = TOKEN_TTL;

/// Access identity and shared client admission budget are separate values.
/// A browser grant narrows access without creating another subject budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    client_budget: String,
    browser_grant: Option<String>,
}

impl Owner {
    pub fn anonymous(address: std::net::IpAddr) -> Self {
        let client_budget = match address.to_canonical() {
            std::net::IpAddr::V4(address) => address.to_string(),
            std::net::IpAddr::V6(address) => ipnet::Ipv6Net::new(address, 64)
                .expect("fixed valid IPv6 prefix")
                .trunc()
                .to_string(),
        };
        Self {
            client_budget,
            browser_grant: None,
        }
    }

    pub fn principal(subject: impl Into<String>) -> Self {
        Self {
            client_budget: format!("principal:{}", subject.into()),
            browser_grant: None,
        }
    }

    pub fn delegated(subject: impl Into<String>, grant_id: impl Into<String>) -> Self {
        Self {
            client_budget: format!("principal:{}", subject.into()),
            browser_grant: Some(grant_id.into()),
        }
    }

    pub fn budget_key(&self) -> &str {
        &self.client_budget
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadError {
    Invalid,
    GlobalFull,
    ClientFull,
    OwnerMismatch,
    RandomUnavailable,
}
impl UploadError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::GlobalFull => "globalFull",
            Self::ClientFull => "clientFull",
            Self::OwnerMismatch => "ownerMismatch",
            Self::RandomUnavailable => "unavailable",
        }
    }
    pub fn status(self) -> http::StatusCode {
        match self {
            Self::Invalid => http::StatusCode::BAD_REQUEST,
            Self::GlobalFull | Self::RandomUnavailable => http::StatusCode::SERVICE_UNAVAILABLE,
            Self::ClientFull => http::StatusCode::TOO_MANY_REQUESTS,
            Self::OwnerMismatch => http::StatusCode::FORBIDDEN,
        }
    }
    pub fn retry(self) -> bool {
        matches!(self, Self::GlobalFull | Self::ClientFull)
    }
}
impl fmt::Display for UploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "unknown upload id",
            Self::GlobalFull => "upload capacity exhausted",
            Self::ClientFull => "client upload capacity exhausted",
            Self::OwnerMismatch => "upload id belongs to another client",
            Self::RandomUnavailable => "upload session mint failed",
        })
    }
}
impl std::error::Error for UploadError {}

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
    entries: Mutex<HashMap<String, Arc<Mutex<Aggregate>>>>,
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
    fn authorize(&self, owner: &Owner) -> Result<(), UploadError> {
        if &self.owner != owner {
            Err(UploadError::OwnerMismatch)
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
    pub fn new() -> Result<Self, UploadError> {
        let mut key = [0; 32];
        random(&mut key)?;
        Ok(Self {
            inner: Arc::new(Store {
                key,
                origin: Instant::now(),
                next_sweep: Mutex::new(Instant::now() + Duration::from_secs(5)),
                entries: Mutex::new(HashMap::new()),
            }),
        })
    }
    /// Tokens authenticate themselves; minting consumes no retained aggregate capacity.
    pub fn mint(&self) -> Result<String, UploadError> {
        let mut raw = [0; 56];
        raw[..8].copy_from_slice(&nanos(self.inner.origin.elapsed()).max(1).to_be_bytes());
        random(&mut raw[8..24])?;
        let mut mac = token_mac(&self.inner.key);
        mac.update(&raw[..24]);
        let tag = mac.finalize().into_bytes();
        raw[24..].copy_from_slice(&tag);
        Ok(format!("gmu_{}", URL_SAFE_NO_PAD.encode(raw)))
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
    ) -> Result<Arc<Mutex<Aggregate>>, UploadError> {
        self.sweep_if_due();
        let mut entries = self.inner.entries.lock().expect("upload store lock");
        let aggregate = if let Some(aggregate) = entries.get(id) {
            aggregate.clone()
        } else {
            if !create || !self.valid(id) {
                return Err(UploadError::Invalid);
            }
            if entries.len() >= MAX_LIVE_UPLOADS {
                return Err(UploadError::GlobalFull);
            }
            if entries
                .values()
                .filter(|entry| {
                    entry
                        .lock()
                        .expect("upload aggregate lock")
                        .owner
                        .budget_key()
                        == owner.budget_key()
                })
                .count()
                >= MAX_UPLOADS_PER_CLIENT
            {
                return Err(UploadError::ClientFull);
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
            entries.insert(id.to_owned(), aggregate.clone());
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
                    return Err(UploadError::Invalid);
                }
                state.lanes += 1;
                state.touched = Instant::now();
            }
        }
        Ok(aggregate)
    }
    pub fn begin(&self, id: &str, owner: &Owner) -> Result<UploadLane, UploadError> {
        Ok(UploadLane {
            aggregate: self.access(id, owner, true, true)?,
            bytes: 0,
        })
    }
    /// Read-only observation never extends retention and never creates an aggregate.
    pub fn checkpoint(&self, id: &str, owner: &Owner) -> Result<UploadCheckpoint, UploadError> {
        Ok(self
            .access(id, owner, false, false)?
            .lock()
            .expect("upload aggregate lock")
            .checkpoint())
    }
    pub fn finish(&self, id: &str, owner: &Owner) -> Result<(), UploadError> {
        let aggregate = self.access(id, owner, false, false)?;
        let mut state = aggregate.lock().expect("upload aggregate lock");
        state.finished = true;
        state.changed.notify_waiters();
        Ok(())
    }
    pub fn subscribe(&self, id: &str, owner: &Owner) -> Result<UploadSubscription, UploadError> {
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
            next_tick: tokio::time::Instant::now() + Duration::from_millis(100),
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
    pub fn sweep(&self) {
        self.sweep_at(Instant::now());
    }
    /// Also permits a caller-owned maintenance loop; no background task is spawned.
    pub fn sweep_at(&self, now: Instant) {
        self.inner
            .entries
            .lock()
            .expect("upload store lock")
            .retain(|_, aggregate| {
                let mut state = aggregate.lock().expect("upload aggregate lock");
                if state.lanes == 0
                    && now.saturating_duration_since(state.touched) > UPLOAD_RETENTION
                {
                    state.expired = true;
                    state.changed.notify_waiters();
                    false
                } else {
                    true
                }
            });
    }
    pub fn retained(&self) -> usize {
        self.inner.entries.lock().expect("upload store lock").len()
    }
}

/// A transport must record only bytes actually received. Dropping its future releases the lane.
pub struct UploadLane {
    aggregate: Arc<Mutex<Aggregate>>,
    bytes: u64,
}
impl UploadLane {
    /// Datagram lanes have no stream FIN. Observe the explicit finish request
    /// without retaining a borrow of the lane while it records incoming bytes.
    pub fn finished(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let aggregate = self.aggregate.clone();
        async move {
            let changed = aggregate
                .lock()
                .expect("upload aggregate lock")
                .changed
                .clone();
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
        state.touched = now;
        self.bytes = self.bytes.saturating_add(bytes as u64);
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}
impl Drop for UploadLane {
    fn drop(&mut self) {
        let mut state = self.aggregate.lock().expect("upload aggregate lock");
        state.lanes -= 1;
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
    next_tick: tokio::time::Instant,
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
                if state.expired
                    || !state
                        .claim
                        .as_ref()
                        .is_some_and(|claim| Arc::ptr_eq(claim, &self.claim))
                {
                    self.ended = true;
                    return None;
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
                    self.next_tick = tokio::time::Instant::now() + Duration::from_millis(100);
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
fn random(bytes: &mut [u8]) -> Result<(), UploadError> {
    crate::crypto::provider()
        .secure_random
        .fill(bytes)
        .map_err(|_| UploadError::RandomUnavailable)
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
                .begin(
                    &signed(nanos(Duration::from_secs(1))),
                    &Owner::principal("a")
                )
                .err(),
            Some(UploadError::Invalid)
        );
        assert_eq!(
            store
                .begin(
                    &signed(nanos(Duration::from_secs(600))),
                    &Owner::principal("a")
                )
                .err(),
            Some(UploadError::Invalid)
        );
        let id = store.mint().unwrap();
        let mut lane = store.begin(&id, &Owner::principal("a")).unwrap();
        lane.record(15);
        Arc::get_mut(&mut store.inner).unwrap().origin -= Duration::from_secs(121);
        assert!(!store.valid(&id));
        assert_eq!(
            store.checkpoint(&id, &Owner::principal("a")).unwrap().bytes,
            15
        );
        drop(store.begin(&id, &Owner::principal("a")).unwrap());
    }

    #[test]
    fn observing_and_finishing_do_not_refresh_idle_retention() {
        let store = UploadStore::new().unwrap();
        let id = store.mint().unwrap();
        drop(store.begin(&id, &Owner::principal("a")).unwrap());
        let aggregate = store.inner.entries.lock().unwrap()[&id].clone();
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
        let aggregate = store.inner.entries.lock().unwrap()[&id].clone();
        let touched = Instant::now() - TOKEN_TTL;
        aggregate.lock().unwrap().touched = touched;

        store.sweep_at(touched + TOKEN_TTL);
        assert_eq!(store.retained(), 1);
        assert_eq!(store.begin(&id, &owner).err(), Some(UploadError::Invalid));
        assert_eq!(
            store.begin(&id, &Owner::principal("other")).err(),
            Some(UploadError::OwnerMismatch)
        );
    }
}

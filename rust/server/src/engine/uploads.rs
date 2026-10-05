//! Upload aggregates (`api/upload.md`): server-minted IDs bound to their owner, capacity with displacement before
//! the first byte, tombstones and retention.

use super::{Meter, ProgressFeed, UploadSink, meter::Transfer};
use crate::{
    lane::Lane,
    limits::{Hold, Quota, Refusal},
    lock,
    peer::{ClientKey, ClientKeys},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_proto::{refusal::UploadRefusal, upload::Counters};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};

/// Aggregates the server holds at once, finished ones until their retention ends.
pub const MAX_LIVE: usize = 1000;
/// Aggregates one client's narrowest key holds; each wider key holds twice the one before it.
pub const MAX_PER_CLIENT: usize = 32;
/// An ID creates state for this long after it was minted.
pub const ID_LIFETIME: Duration = Duration::from_secs(120);
/// An aggregate without lanes is kept this long after its last lane joined or left, so it outlives its ID.
pub const RETENTION: Duration = ID_LIFETIME;
/// Retention is swept at most this often.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);
const SWEEP_NANOS: u64 = SWEEP_INTERVAL.as_secs() * 1_000_000_000;

const ID_PREFIX: &str = "gmu_";
/// An ID signs its issue time and 16 random bytes.
const SIGNED_BYTES: usize = 24;
const ID_BYTES: usize = SIGNED_BYTES + 32;

/// The upload store; clones share it.
#[derive(Clone)]
pub struct Uploads(Arc<Store>);

struct Store {
    key: [u8; 32],
    meter: Meter,
    clock: Clock,
    capacity: Quota,
    /// The store clock at the next sweep, read without locking `entries`.
    next_sweep: AtomicU64,
    entries: Mutex<Entries>,
}

struct Entries {
    live: HashMap<Box<str>, Entry>,
    /// Displaced IDs, refused until they would have expired.
    tombstones: HashMap<Box<str>, Instant>,
}

struct Entry {
    aggregate: Arc<Aggregate>,
    _capacity: Hold,
}

/// The store's monotonic clock in nanoseconds, starting at one so that zero means unset.
#[derive(Clone, Copy)]
struct Clock(Instant);

impl Clock {
    fn now(self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos())
            .unwrap_or(u64::MAX - 1)
            .saturating_add(1)
    }
}

/// One upload ID's receiver: the bytes of all its lanes and the time since its first accepted chunk.
pub(super) struct Aggregate {
    owner: ClientKey,
    clock: Clock,
    bytes: AtomicU64,
    /// The clock at the first accepted chunk; zero before it.
    first: AtomicU64,
    life: Mutex<Life>,
    /// Woken when a lane leaves, the upload finishes or expires, or a reader replaces another.
    pub(super) changed: Notify,
}

pub(super) struct Life {
    pub(super) lanes: usize,
    touched: Instant,
    pub(super) finished: bool,
    pub(super) expired: bool,
    /// The number of the reader whose feed is current.
    pub(super) reader: u64,
}

impl Aggregate {
    pub(super) fn life(&self) -> MutexGuard<'_, Life> {
        lock(&self.life)
    }

    pub(super) fn record(&self, bytes: u64) {
        if self.first.load(Ordering::Relaxed) == 0 {
            let _ = self
                .first
                .compare_exchange(0, self.clock.now(), Ordering::Relaxed, Ordering::Relaxed);
        }
        self.bytes.fetch_add(bytes, Ordering::Release);
    }

    pub(super) fn counters(&self) -> Counters {
        let bytes = self.bytes.load(Ordering::Acquire);
        let nanos = match self.first.load(Ordering::Relaxed) {
            0 => 0,
            first => self.clock.now().saturating_sub(first),
        };
        Counters::new(bytes, nanos)
    }

    /// Finalized or expired: no lane joins any more.
    pub(super) fn ended(&self) -> bool {
        let life = self.life();
        life.finished || life.expired
    }

    /// A lane left: retention starts again from now.
    pub(super) fn leave(&self) {
        let mut life = self.life();
        life.lanes -= 1;
        life.touched = Instant::now();
        drop(life);
        self.changed.notify_waiters();
    }

    fn join(&self) -> Result<(), UploadRefusal> {
        let mut life = self.life();
        if life.finished {
            return Err(UploadRefusal::Invalid);
        }
        life.lanes += 1;
        life.touched = Instant::now();
        Ok(())
    }

    fn expire(&self) {
        self.life().expired = true;
        self.changed.notify_waiters();
    }
}

impl Entries {
    fn sweep(&mut self, now: Instant) {
        self.tombstones.retain(|_, until| *until >= now);
        self.live.retain(|_, entry| {
            let life = entry.aggregate.life();
            let kept = life.lanes > 0 || now.saturating_duration_since(life.touched) <= RETENTION;
            drop(life);
            if !kept {
                entry.aggregate.expire();
            }
            kept
        });
    }

    /// Expires the stalest aggregate without lanes, bytes or finish, refusing its ID while it could be used.
    fn displace(&mut self, now: Instant) -> bool {
        if self.tombstones.len() >= MAX_LIVE {
            return false;
        }
        let victim = self
            .live
            .iter()
            .filter_map(|(id, entry)| {
                let life = entry.aggregate.life();
                let empty = life.lanes == 0 && !life.finished && entry.aggregate.bytes.load(Ordering::Acquire) == 0;
                empty.then_some((life.touched, id))
            })
            .min_by_key(|(touched, _)| *touched)
            .map(|(_, id)| id.clone());
        let Some(id) = victim else { return false };
        if let Some(entry) = self.live.remove(&id) {
            entry.aggregate.expire();
        }
        self.tombstones.insert(id, now + ID_LIFETIME);
        true
    }
}

/// How a request reaches an aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    /// A data lane, which may create the aggregate.
    Join,
    /// A progress reader, which may create it.
    Watch,
    /// A checkpoint or finish, which needs it to exist.
    Read,
}

impl Uploads {
    /// A store whose IDs `key` signs; `meter` counts its lanes' bytes.
    pub fn new(key: [u8; 32], meter: Meter) -> Self {
        let now = Instant::now();
        Self(Arc::new(Store {
            key,
            meter,
            clock: Clock(now),
            capacity: Quota::new(MAX_LIVE, MAX_PER_CLIENT),
            next_sweep: AtomicU64::new(SWEEP_NANOS),
            entries: Mutex::new(Entries { live: HashMap::new(), tombstones: HashMap::new() }),
        }))
    }

    /// A new ID, which holds no state until a lane or reader uses it.
    pub fn mint(&self) -> String {
        let mut id = [0; ID_BYTES];
        id[..8].copy_from_slice(&self.0.clock.now().to_be_bytes());
        id[8..SIGNED_BYTES].copy_from_slice(&crate::random::<16>());
        let tag = self.mac(&id[..SIGNED_BYTES]).finalize().into_bytes();
        id[SIGNED_BYTES..].copy_from_slice(&tag);
        format!("{ID_PREFIX}{}", URL_SAFE_NO_PAD.encode(id))
    }

    /// Joins `id`'s aggregate as a data lane that `lane` bounds, its bytes counted in `transfer`.
    pub fn begin(
        &self,
        id: &str,
        owner: Option<&ClientKeys>,
        lane: Lane,
        transfer: Option<Arc<Transfer>>,
    ) -> Result<UploadSink, UploadRefusal> {
        let aggregate = self.access(id, owner, Access::Join)?;
        Ok(UploadSink::new(aggregate, lane, transfer))
    }

    pub fn meter(&self) -> &Meter {
        &self.0.meter
    }

    /// Attaches the progress feed, replacing the previous reader's.
    pub fn subscribe(&self, id: &str, owner: Option<&ClientKeys>) -> Result<ProgressFeed, UploadRefusal> {
        let aggregate = self.access(id, owner, Access::Watch)?;
        let reader = {
            let mut life = aggregate.life();
            life.reader += 1;
            life.reader
        };
        aggregate.changed.notify_waiters();
        Ok(ProgressFeed::new(self.clone(), aggregate, reader))
    }

    /// An existing aggregate's counters, without refreshing its retention.
    pub fn checkpoint(&self, id: &str, owner: Option<&ClientKeys>) -> Result<Counters, UploadRefusal> {
        Ok(self.access(id, owner, Access::Read)?.counters())
    }

    /// Finalizes an aggregate: it takes no new lane, and its feed completes once its lanes drain.
    pub fn finish(&self, id: &str, owner: Option<&ClientKeys>) -> Result<(), UploadRefusal> {
        let aggregate = self.access(id, owner, Access::Read)?;
        aggregate.life().finished = true;
        aggregate.changed.notify_waiters();
        Ok(())
    }

    /// Expires what retention no longer covers once a sweep is due; until then it reads one atomic.
    pub(super) fn sweep(&self) {
        if self.0.clock.now() >= self.0.next_sweep.load(Ordering::Relaxed) {
            drop(self.entries());
        }
    }

    fn access(&self, id: &str, owner: Option<&ClientKeys>, access: Access) -> Result<Arc<Aggregate>, UploadRefusal> {
        let narrowest = owner.and_then(ClientKeys::narrowest);
        if access != Access::Read && narrowest.is_none() {
            return Err(UploadRefusal::OwnerMismatch);
        }
        let mut entries = self.entries();
        if let Some(entry) = entries.live.get(id) {
            if narrowest.as_ref() != Some(&entry.aggregate.owner) {
                return Err(UploadRefusal::OwnerMismatch);
            }
            if access == Access::Join {
                entry.aggregate.join()?;
            }
            return Ok(entry.aggregate.clone());
        }
        let (Some(keys), Some(owner)) = (owner, narrowest) else {
            return Err(UploadRefusal::Invalid);
        };
        if access == Access::Read || entries.tombstones.contains_key(id) || !self.valid(id) {
            return Err(UploadRefusal::Invalid);
        }
        let capacity = self.capacity(&mut entries, keys)?;
        let aggregate = Arc::new(Aggregate {
            owner,
            clock: self.0.clock,
            bytes: AtomicU64::new(0),
            first: AtomicU64::new(0),
            life: Mutex::new(Life {
                lanes: usize::from(access == Access::Join),
                touched: Instant::now(),
                finished: false,
                expired: false,
                reader: 0,
            }),
            changed: Notify::new(),
        });
        entries
            .live
            .insert(id.into(), Entry { aggregate: aggregate.clone(), _capacity: capacity });
        Ok(aggregate)
    }

    /// A share of the capacity; at the limit an empty aggregate makes room.
    fn capacity(&self, entries: &mut Entries, keys: &ClientKeys) -> Result<Hold, UploadRefusal> {
        let refusal = |refusal| match refusal {
            Refusal::Client => UploadRefusal::ClientFull,
            Refusal::Total => UploadRefusal::GlobalFull,
        };
        match self.0.capacity.acquire(keys, 1) {
            Err(Refusal::Total) if entries.displace(Instant::now()) => {
                self.0.capacity.acquire(keys, 1).map_err(refusal)
            }
            result => result.map_err(refusal),
        }
    }

    fn entries(&self) -> MutexGuard<'_, Entries> {
        let mut entries = lock(&self.0.entries);
        let now = self.0.clock.now();
        if now >= self.0.next_sweep.load(Ordering::Relaxed) {
            entries.sweep(Instant::now());
            self.0.next_sweep.store(now + SWEEP_NANOS, Ordering::Relaxed);
        }
        entries
    }

    /// Whether this store minted `id` within its lifetime.
    fn valid(&self, id: &str) -> bool {
        let mut raw = [0; ID_BYTES];
        let decoded = id
            .strip_prefix(ID_PREFIX)
            .filter(|encoded| encoded.len() == (ID_BYTES * 4).div_ceil(3))
            .is_some_and(|encoded| URL_SAFE_NO_PAD.decode_slice(encoded, &mut raw) == Ok(ID_BYTES));
        if !decoded
            || self
                .mac(&raw[..SIGNED_BYTES])
                .verify_slice(&raw[SIGNED_BYTES..])
                .is_err()
        {
            return false;
        }
        let issued = u64::from_be_bytes(raw[..8].try_into().expect("eight bytes"));
        let lifetime = u64::try_from(ID_LIFETIME.as_nanos()).expect("two minutes in nanoseconds");
        self.0
            .clock
            .now()
            .checked_sub(issued)
            .is_some_and(|age| age <= lifetime)
    }

    fn mac(&self, signed: &[u8]) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0.key).expect("HMAC takes keys of any length");
        mac.update(signed);
        mac
    }

    #[cfg(test)]
    pub(super) fn live(&self) -> usize {
        self.entries().live.len()
    }
}

#[cfg(test)]
#[path = "uploads_tests.rs"]
mod tests;

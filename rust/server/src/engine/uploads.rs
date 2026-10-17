//! Upload aggregates (`api/upload.md`): owner-bound minted IDs, displacement before the first byte, retention.

use super::{Meter, ProgressFeed, Transfer, UploadSink};
use crate::{
    lane::Lane,
    limits::{Hold, Quota, Refusal},
    lock,
    peer::{ClientKey, ClientKeys},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_proto::{refusal::UploadRefusal, upload::Counters};
use ring::hmac;
use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};

/// Aggregates the server holds at once, finished ones until their retention ends.
const MAX_LIVE: usize = 1000;
/// Aggregates one client's narrowest key holds; each wider key holds twice the one before it.
const MAX_PER_CLIENT: usize = 32;
/// An ID creates state for this long after it was minted.
const ID_LIFETIME: Duration = Duration::from_mins(2);
const ID_LIFETIME_NANOS: u64 = ID_LIFETIME.as_nanos() as u64;
/// An aggregate without lanes is kept this long after its last lane joined or left, so it outlives its ID.
const RETENTION: Duration = ID_LIFETIME;
/// Retention is swept at most this often.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);
const SWEEP_NANOS: u64 = SWEEP_INTERVAL.as_nanos() as u64;

const ID_PREFIX: &str = "gmu_";
/// An ID signs its issue time and 16 random bytes.
const SIGNED_BYTES: usize = 24;
const ID_BYTES: usize = SIGNED_BYTES + 32;

/// The upload store; clones share it.
#[derive(Clone)]
pub struct Uploads(Arc<Store>);

struct Store {
    key: hmac::Key,
    meter: Meter,
    clock: Clock,
    capacity: Quota,
    /// The store clock at the next sweep, read without locking `entries`.
    next_sweep: AtomicU64,
    entries: Mutex<Entries>,
}

struct Entries {
    live: HashMap<UploadId, Entry>,
    /// Displaced IDs, refused until they would have expired.
    tombstones: HashMap<UploadId, Instant>,
}

/// An upload ID decoded from its text: issue time and nonce, then their HMAC tag.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct UploadId([u8; ID_BYTES]);

impl UploadId {
    fn parse(text: &str) -> Option<Self> {
        let encoded = text.strip_prefix(ID_PREFIX)?;
        let mut raw = [0; ID_BYTES];
        let exact = encoded.len() == (ID_BYTES * 4).div_ceil(3);
        (exact && URL_SAFE_NO_PAD.decode_slice(encoded, &mut raw) == Ok(ID_BYTES)).then_some(Self(raw))
    }
}

impl fmt::Display for UploadId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{ID_PREFIX}{}", URL_SAFE_NO_PAD.encode(self.0))
    }
}

struct Entry {
    aggregate: Arc<Aggregate>,
    _capacity: Hold,
}

/// The store's monotonic clock in nanoseconds.
#[derive(Clone, Copy)]
struct Clock(Instant);

impl Clock {
    fn now(self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// One upload ID's receiver: the bytes of all its lanes and the time since its first accepted chunk.
pub(super) struct Aggregate {
    owner: ClientKey,
    clock: Clock,
    bytes: AtomicU64,
    /// The clock at the first accepted chunk.
    first: OnceLock<u64>,
    life: Mutex<Life>,
    /// Woken when a lane leaves, the upload finishes or expires, or a reader replaces another.
    pub(super) changed: Notify,
}

pub(super) struct Life {
    pub(super) lanes: usize,
    touched: Instant,
    pub(super) phase: Phase,
    /// The number of the reader whose feed is current.
    pub(super) reader: u64,
}

/// Where an upload is: taking lanes, finalized, or expired, which ends a finalized one too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Open,
    Finished,
    Expired,
}

impl Aggregate {
    pub(super) fn life(&self) -> MutexGuard<'_, Life> {
        lock(&self.life)
    }

    pub(super) fn record(&self, bytes: u64) {
        self.first.get_or_init(|| self.clock.now());
        self.bytes.fetch_add(bytes, Ordering::Release);
    }

    pub(super) fn counters(&self) -> Counters {
        let bytes = self.bytes.load(Ordering::Acquire);
        let nanos = self
            .first
            .get()
            .map_or(0, |first| self.clock.now().saturating_sub(*first));
        Counters::new(bytes, nanos)
    }

    /// Finalized or expired: no lane joins any more.
    pub(super) fn ended(&self) -> bool {
        self.life().phase != Phase::Open
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
        if life.phase != Phase::Open {
            return Err(UploadRefusal::Invalid);
        }
        life.lanes += 1;
        life.touched = Instant::now();
        Ok(())
    }

    fn expire(&self) {
        self.life().phase = Phase::Expired;
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
                let empty =
                    life.lanes == 0 && life.phase == Phase::Open && entry.aggregate.bytes.load(Ordering::Acquire) == 0;
                empty.then_some((life.touched, id))
            })
            .min_by_key(|(touched, _)| *touched)
            .map(|(_, id)| *id);
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
enum Reach {
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
            key: hmac::Key::new(hmac::HMAC_SHA256, &key),
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
        let tag = hmac::sign(&self.0.key, &id[..SIGNED_BYTES]);
        id[SIGNED_BYTES..].copy_from_slice(tag.as_ref());
        UploadId(id).to_string()
    }

    /// Joins `id`'s aggregate as a data lane that `lane` bounds, its bytes counted in `transfer`.
    pub fn begin(
        &self,
        id: &str,
        owner: Option<&ClientKeys>,
        lane: Lane,
        transfer: Option<Arc<Transfer>>,
    ) -> Result<UploadSink, UploadRefusal> {
        let aggregate = self.reach(id, owner, Reach::Join)?;
        Ok(UploadSink::new(aggregate, lane, transfer))
    }

    pub fn meter(&self) -> &Meter {
        &self.0.meter
    }

    /// Attaches the progress feed, replacing the previous reader's.
    pub fn subscribe(&self, id: &str, owner: Option<&ClientKeys>) -> Result<ProgressFeed, UploadRefusal> {
        let aggregate = self.reach(id, owner, Reach::Watch)?;
        let reader = {
            let mut life = aggregate.life();
            life.reader += 1;
            life.reader
        };
        aggregate.changed.notify_waiters();
        Ok(ProgressFeed::new(self.clone(), aggregate, reader))
    }

    /// Whether `subscribe` would attach the feed, creating and replacing nothing.
    pub fn watchable(&self, id: &str, owner: Option<&ClientKeys>) -> Result<(), UploadRefusal> {
        let narrowest = owner
            .and_then(ClientKeys::narrowest)
            .ok_or(UploadRefusal::OwnerMismatch)?;
        let id = UploadId::parse(id).ok_or(UploadRefusal::Invalid)?;
        let entries = self.entries();
        match entries.live.get(&id) {
            Some(entry) if entry.aggregate.owner != narrowest => Err(UploadRefusal::OwnerMismatch),
            Some(_) => Ok(()),
            None if entries.tombstones.contains_key(&id) || !self.valid(&id) => Err(UploadRefusal::Invalid),
            None => Ok(()),
        }
    }

    /// An existing aggregate's counters, without refreshing its retention.
    pub fn checkpoint(&self, id: &str, owner: Option<&ClientKeys>) -> Result<Counters, UploadRefusal> {
        Ok(self.reach(id, owner, Reach::Read)?.counters())
    }

    /// Finalizes an aggregate: it takes no new lane, and its feed completes once its lanes drain.
    pub fn finish(&self, id: &str, owner: Option<&ClientKeys>) -> Result<(), UploadRefusal> {
        let aggregate = self.reach(id, owner, Reach::Read)?;
        let mut life = aggregate.life();
        if life.phase == Phase::Open {
            life.phase = Phase::Finished;
        }
        drop(life);
        aggregate.changed.notify_waiters();
        Ok(())
    }

    /// Expires what retention no longer covers once a sweep is due; until then it reads one atomic.
    pub(super) fn sweep(&self) {
        if self.0.clock.now() >= self.0.next_sweep.load(Ordering::Relaxed) {
            drop(self.entries());
        }
    }

    fn reach(&self, id: &str, owner: Option<&ClientKeys>, reach: Reach) -> Result<Arc<Aggregate>, UploadRefusal> {
        let narrowest = owner.and_then(ClientKeys::narrowest);
        if reach != Reach::Read && narrowest.is_none() {
            return Err(UploadRefusal::OwnerMismatch);
        }
        let id = UploadId::parse(id).ok_or(UploadRefusal::Invalid)?;
        let mut entries = self.entries();
        if let Some(entry) = entries.live.get(&id) {
            if narrowest.as_ref() != Some(&entry.aggregate.owner) {
                return Err(UploadRefusal::OwnerMismatch);
            }
            if reach == Reach::Join {
                entry.aggregate.join()?;
            }
            return Ok(entry.aggregate.clone());
        }
        let (Some(keys), Some(owner)) = (owner, narrowest) else {
            return Err(UploadRefusal::Invalid);
        };
        if reach == Reach::Read || entries.tombstones.contains_key(&id) || !self.valid(&id) {
            return Err(UploadRefusal::Invalid);
        }
        let capacity = self.capacity(&mut entries, keys)?;
        let aggregate = Arc::new(Aggregate {
            owner,
            clock: self.0.clock,
            bytes: AtomicU64::new(0),
            first: OnceLock::new(),
            life: Mutex::new(Life {
                lanes: usize::from(reach == Reach::Join),
                touched: Instant::now(),
                phase: Phase::Open,
                reader: 0,
            }),
            changed: Notify::new(),
        });
        let entry = Entry { aggregate: aggregate.clone(), _capacity: capacity };
        entries.live.insert(id, entry);
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
    fn valid(&self, UploadId(raw): &UploadId) -> bool {
        let (signed, tag) = raw.split_at(SIGNED_BYTES);
        if hmac::verify(&self.0.key, signed, tag).is_err() {
            return false;
        }
        let issued = raw.first_chunk().map(|issued| u64::from_be_bytes(*issued));
        let age = issued.and_then(|issued| self.0.clock.now().checked_sub(issued));
        age.is_some_and(|age| age <= ID_LIFETIME_NANOS)
    }
}

#[cfg(test)]
#[path = "uploads_tests.rs"]
mod tests;

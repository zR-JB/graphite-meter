//! The upload store's tests, which reach its internals.
use super::*;
use graphite_meter_core::wire::{decode_upload_progress, encode_upload_progress};

impl UploadStore {
    fn new() -> Option<Self> {
        Self::with_meter(crate::meter::Meter::default())
    }

    pub(crate) fn retained(&self) -> usize {
        lock(&self.inner.entries).by_id.len()
    }

    fn aggregate(&self, id: &str) -> Arc<Mutex<Aggregate>> {
        lock(&self.inner.entries).by_id[id].clone()
    }

    /// A new upload `owner` joins and leaves.
    fn joined(&self, owner: &Owner) {
        drop(self.begin(&self.mint().unwrap(), owner).unwrap());
    }

    fn expire(&self) {
        self.sweep_at(Instant::now() + UPLOAD_RETENTION + Duration::from_secs(1));
    }
}

impl Owner {
    fn principal(subject: impl Into<String>) -> Self {
        Self(vec![Self::principal_key(&subject.into())])
    }
}

#[test]
fn token_age_is_checked_only_when_creating_an_aggregate() {
    let mut store = UploadStore::new().unwrap();
    let owner = Owner::principal("a");
    Arc::get_mut(&mut store.inner).unwrap().origin = Instant::now() - Duration::from_secs(300);
    let signed = |issued: u64| {
        let mut raw = [0; 56];
        raw[..8].copy_from_slice(&issued.to_be_bytes());
        let mut mac = token_mac(&store.inner.key);
        mac.update(&raw[..24]);
        raw[24..].copy_from_slice(&mac.finalize().into_bytes());
        format!("gmu_{}", URL_SAFE_NO_PAD.encode(raw))
    };
    for issued in [1, 600] {
        let id = signed(nanos(Duration::from_secs(issued)));
        assert_eq!(store.begin(&id, &owner).err(), Some(UploadRefusal::Invalid));
    }
    let id = store.mint().unwrap();
    let mut lane = store.begin(&id, &owner).unwrap();
    lane.record(15);
    Arc::get_mut(&mut store.inner).unwrap().origin -= Duration::from_secs(121);
    assert!(!store.valid(&id));
    assert_eq!(store.checkpoint(&id, &owner).unwrap().bytes, 15);
    drop(store.begin(&id, &owner).unwrap());
}

#[test]
fn observing_and_finishing_do_not_refresh_idle_retention() {
    let store = UploadStore::new().unwrap();
    let owner = Owner::principal("a");
    let id = store.mint().unwrap();
    drop(store.begin(&id, &owner).unwrap());
    let aggregate = store.aggregate(&id);
    let old = Instant::now() - Duration::from_secs(80);
    lock(&aggregate).touched = old;
    store.checkpoint(&id, &owner).unwrap();
    let _subscription = store.subscribe(&id, &owner).unwrap();
    store.finish(&id, &owner).unwrap();
    assert_eq!(lock(&aggregate).touched, old);
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
    let touched = Instant::now() - TOKEN_TTL;
    lock(&store.aggregate(&id)).touched = touched;

    store.sweep_at(touched + TOKEN_TTL);
    assert_eq!(store.retained(), 1);
    assert_eq!(store.begin(&id, &owner).err(), Some(UploadRefusal::Invalid));
    let other = Owner::principal("other");
    assert_eq!(store.begin(&id, &other).err(), Some(UploadRefusal::OwnerMismatch));
}

#[test]
fn an_unresolved_owner_creates_and_reaches_nothing() {
    let store = UploadStore::new().unwrap();
    let id = store.mint().unwrap();
    let nobody = Owner::unresolved();
    // As Go's accessFor, joining refuses the owner before looking at the ID.
    let joined = store.begin("invalid", &nobody);
    assert_eq!(joined.err(), Some(UploadRefusal::OwnerMismatch));
    assert_eq!(store.subscribe(&id, &nobody).err(), Some(UploadRefusal::OwnerMismatch));
    assert_eq!(store.retained(), 0);
    drop(store.begin(&id, &Owner::principal("a")).unwrap());
    assert_eq!(store.checkpoint(&id, &nobody).err(), Some(UploadRefusal::OwnerMismatch));
    assert_eq!(store.finish(&id, &nobody).err(), Some(UploadRefusal::OwnerMismatch));
    let fresh = store.mint().unwrap();
    assert_eq!(store.checkpoint(&fresh, &nobody).err(), Some(UploadRefusal::Invalid));
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
    store.joined(&other);
    lock(&store.aggregate(&first.unwrap())).touched = Instant::now() - UPLOAD_RETENTION - Duration::from_secs(1);
    store.sweep_at(Instant::now());

    assert_eq!(store.retained(), MAX_UPLOADS_PER_CLIENT);
    store.joined(&owner);
    let id = store.mint().unwrap();
    assert_eq!(store.begin(&id, &owner).err(), Some(UploadRefusal::ClientFull));
    store.joined(&other);
}

#[test]
fn tokens_are_stateless_authenticated_and_store_local() {
    let store = UploadStore::new().unwrap();
    let other = UploadStore::new().unwrap();
    let owner = Owner::principal("a");
    for _ in 0..1100 {
        let id = store.mint().unwrap();
        assert_eq!(id.len(), 79);
        assert!(id.starts_with("gmu_"));
    }
    assert_eq!(store.retained(), 0);
    let id = store.mint().unwrap();
    assert_eq!(other.begin(&id, &owner).err(), Some(UploadRefusal::Invalid));
    let mut forged = id.clone().into_bytes();
    forged[10] = if forged[10] == b'A' { b'B' } else { b'A' };
    let forged = String::from_utf8(forged).unwrap();
    assert_eq!(store.begin(&forged, &owner).err(), Some(UploadRefusal::Invalid));
    assert_eq!(store.checkpoint(&id, &owner), Err(UploadRefusal::Invalid));
    drop(store.begin(&id, &owner).unwrap());
    assert_eq!(store.retained(), 1);
}

#[test]
fn delegated_owners_share_capacity_but_not_access() {
    let store = UploadStore::new().unwrap();
    let id = store.mint().unwrap();
    let browser = |grant: u8| Owner::delegated("a", &format!("browser:{grant}"));
    drop(store.begin(&id, &browser(1)).unwrap());
    assert_eq!(store.begin(&id, &browser(2)).err(), Some(UploadRefusal::OwnerMismatch));
    assert_eq!(store.checkpoint(&id, &browser(2)), Err(UploadRefusal::OwnerMismatch));
    assert_eq!(store.finish(&id, &browser(2)), Err(UploadRefusal::OwnerMismatch));
    let subscribed = store.subscribe(&id, &browser(2));
    assert_eq!(subscribed.err(), Some(UploadRefusal::OwnerMismatch));
    for _ in 1..MAX_UPLOADS_PER_CLIENT {
        store.joined(&browser(2));
    }
    for _ in 0..MAX_UPLOADS_PER_CLIENT {
        store.joined(&browser(3));
    }
    for owner in [browser(4), Owner::principal("a")] {
        let id = store.mint().unwrap();
        assert_eq!(store.begin(&id, &owner).err(), Some(UploadRefusal::ClientFull));
    }
    store.joined(&Owner::principal("b"));
    store.expire();
    assert_eq!(store.retained(), 0);
    store.joined(&browser(1));
}

#[test]
fn global_capacity_is_bounded() {
    let store = UploadStore::new().unwrap();
    for index in 0..MAX_LIVE_UPLOADS {
        let mut lane = store
            .begin(&store.mint().unwrap(), &Owner::principal(index.to_string()))
            .unwrap();
        lane.record(1);
    }
    let newcomer = Owner::principal("new-client");
    let id = store.mint().unwrap();
    assert_eq!(store.begin(&id, &newcomer).err(), Some(UploadRefusal::GlobalFull));
    store.expire();
    store.joined(&newcomer);
}

#[tokio::test(start_paused = true)]
async fn concurrent_lanes_credit_each_received_chunk_once() {
    let store = UploadStore::new().unwrap();
    let owner = Owner::principal("a");
    let id = store.mint().unwrap();
    let runtime = tokio::runtime::Handle::current();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let (store, id, owner) = (&store, &id, &owner);
            let runtime = runtime.clone();
            scope.spawn(move || {
                let _runtime = runtime.enter();
                let mut lane = store.begin(id, owner).unwrap();
                for _ in 0..500 {
                    lane.record(64);
                }
                assert_eq!(lane.bytes(), 32_000);
            });
        }
    });
    let before = store.checkpoint(&id, &owner).unwrap();
    assert_eq!(before.bytes, 256_000);
    tokio::time::advance(Duration::from_millis(10)).await;
    let after = store.checkpoint(&id, &owner).unwrap();
    assert_eq!(after.bytes, before.bytes);
    let stalled = after.nanos - before.nanos;
    assert!(stalled >= 10_000_000, "stalls stay in the single aggregate clock");
}

#[tokio::test(start_paused = true)]
async fn completion_waits_for_lane_drop_and_replays_receiver_totals() {
    let store = UploadStore::new().unwrap();
    let owner = Owner::principal("a");
    let id = store.mint().unwrap();
    let mut subscription = store.subscribe(&id, &owner).unwrap();
    assert_eq!(subscription.next().await, Some(UploadProgress::Ready));
    let mut first = store.begin(&id, &owner).unwrap();
    let mut second = store.begin(&id, &owner).unwrap();
    first.record(100);
    second.record(200);
    store.finish(&id, &owner).unwrap();
    assert_eq!(
        store.begin(&id, &owner).err(),
        Some(UploadRefusal::Invalid),
        "a late lane cannot change a terminal receiver total"
    );
    drop(first);
    let pending = tokio::time::timeout(Duration::from_millis(20), subscription.next()).await;
    assert!(pending.is_err());
    second.record(30);
    drop(second);
    let complete = subscription.next().await.unwrap();
    assert!(matches!(complete, UploadProgress::Complete { bytes: 330, nanos } if nanos > 0));
    let encoded = encode_upload_progress(&complete).unwrap();
    assert_eq!(decode_upload_progress(encoded.as_bytes()).unwrap(), complete);
    assert_eq!(subscription.next().await, None);
    assert_eq!(store.begin(&id, &owner).err(), Some(UploadRefusal::Invalid));
    assert_eq!(store.checkpoint(&id, &owner).unwrap().bytes, 330);
    let mut replay = store.subscribe(&id, &owner).unwrap();
    assert_eq!(replay.next().await, Some(UploadProgress::Ready));
    assert!(matches!(
        replay.next().await,
        Some(UploadProgress::Complete { bytes: 330, .. })
    ));
}

#[tokio::test]
async fn replacing_a_subscriber_and_dropping_stale_claim_preserves_current_claim() {
    let store = UploadStore::new().unwrap();
    let owner = Owner::principal("a");
    let id = store.mint().unwrap();
    let mut first = store.subscribe(&id, &owner).unwrap();
    first.next().await;
    let mut second = store.subscribe(&id, &owner).unwrap();
    assert_eq!(first.next().await, None);
    drop(first);
    assert_eq!(second.next().await, Some(UploadProgress::Ready));
    let mut third = store.subscribe(&id, &owner).unwrap();
    assert_eq!(second.next().await, None);
    drop(second);
    assert_eq!(third.next().await, Some(UploadProgress::Ready));
    store.finish(&id, &owner).unwrap();
    let complete = third.next().await;
    assert_eq!(complete, Some(UploadProgress::Complete { bytes: 0, nanos: 0 }));
}

#[tokio::test]
async fn retention_preserves_active_lanes_and_expires_observers() {
    let store = UploadStore::new().unwrap();
    let owner = Owner::principal("a");
    let id = store.mint().unwrap();
    let mut lane = store.begin(&id, &owner).unwrap();
    lane.record(42);
    let mut observer = store.subscribe(&id, &owner).unwrap();
    observer.next().await;
    store.expire();
    assert_eq!(store.retained(), 1);
    assert_eq!(store.checkpoint(&id, &owner).unwrap().bytes, 42);
    drop(lane);
    store.expire();
    assert_eq!(store.retained(), 0);
    assert!(matches!(observer.next().await, Some(UploadProgress::Error { code, .. }) if code == "invalid"));
    assert_eq!(observer.next().await, None);
    assert_eq!(store.checkpoint(&id, &owner), Err(UploadRefusal::Invalid));
}

// Poll directly with a recording waker: lifecycle must wake an already-suspended
// feed and complete on its next poll, without advancing the progress timer.
struct WakeFlag(std::sync::atomic::AtomicBool);
impl std::task::Wake for WakeFlag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn lifecycle_changes_wake_without_waiting_for_progress_tick() {
    use std::{
        future::Future,
        sync::atomic::{AtomicBool, Ordering},
        task::{Context, Poll, Waker},
    };
    let store = UploadStore::new().unwrap();
    let owner = Owner::principal("a");
    let id = store.mint().unwrap();
    let mut lane = store.begin(&id, &owner).unwrap();
    lane.record(25);
    let mut subscription = store.subscribe(&id, &owner).unwrap();
    assert_eq!(subscription.next().await, Some(UploadProgress::Ready));
    let flag = Arc::new(WakeFlag(AtomicBool::new(false)));
    let waker = Waker::from(flag.clone());
    let mut context = Context::from_waker(&waker);
    let woken = || flag.0.swap(false, Ordering::SeqCst);
    let mut pending = Box::pin(subscription.next());
    assert!(pending.as_mut().poll(&mut context).is_pending());
    store.finish(&id, &owner).unwrap();
    assert!(woken(), "finish must wake the feed");
    let delayed = pending.as_mut().poll(&mut context).is_pending();
    assert!(delayed, "live lane must delay completion");
    drop(lane);
    assert!(woken(), "lane drop must wake the feed");
    assert!(matches!(
        pending.as_mut().poll(&mut context),
        Poll::Ready(Some(UploadProgress::Complete { bytes: 25, .. }))
    ));
    drop(pending);

    let id = store.mint().unwrap();
    let mut first = store.subscribe(&id, &owner).unwrap();
    first.next().await;
    let mut pending = Box::pin(first.next());
    assert!(pending.as_mut().poll(&mut context).is_pending());
    let mut replacement = store.subscribe(&id, &owner).unwrap();
    assert!(woken(), "replacement must wake the old feed");
    assert!(matches!(pending.as_mut().poll(&mut context), Poll::Ready(None)));
    drop(pending);
    replacement.next().await;
    let mut pending = Box::pin(replacement.next());
    assert!(pending.as_mut().poll(&mut context).is_pending());
    store.expire();
    assert!(woken(), "expiry must wake the feed");
    assert!(
        matches!(pending.as_mut().poll(&mut context), Poll::Ready(Some(UploadProgress::Error { code, .. })) if code == "invalid")
    );
}

#[test]
fn owner_fields_cannot_collide_through_delimiters() {
    let store = UploadStore::new().unwrap();
    let subject_with_delimiter = Owner::principal("a\0browser:b");
    let delegated = Owner::delegated("a", "browser:b");
    assert_ne!(subject_with_delimiter, delegated);
    assert_ne!(subject_with_delimiter.client_keys()[0], delegated.client_keys()[0]);
    let first = Owner::delegated("a\0b", "c");
    let second = Owner::delegated("a", "b\0c");
    assert_ne!(first, second);
    let id = store.mint().unwrap();
    drop(store.begin(&id, &first).unwrap());
    assert_eq!(store.checkpoint(&id, &second), Err(UploadRefusal::OwnerMismatch));
    let principal = Owner::principal("a");
    assert_eq!(principal.client_keys()[0], delegated.client_keys()[1]);
    let id = store.mint().unwrap();
    drop(store.begin(&id, &principal).unwrap());
    assert_eq!(store.checkpoint(&id, &delegated), Err(UploadRefusal::OwnerMismatch));
    assert_ne!(Owner::delegated("a", ""), principal);
}

#[test]
fn anonymous_owners_canonicalize_ipv4_and_share_ipv6_prefix() {
    let owner = |address: &str| Owner::anonymous(address.parse().unwrap());
    let ipv4 = owner("192.0.2.1");
    assert_eq!(ipv4, owner("::ffff:192.0.2.1"));
    assert_eq!(ipv4.client_keys()[0], "192.0.2.1");
    let first = owner("2001:db8:1:2::1");
    assert_eq!(first, owner("2001:db8:1:2:ffff::1234"));
    assert_eq!(first.client_keys()[0], "2001:db8:1:2::/64");
    assert_ne!(first, owner("2001:db8:1:3::1"));
    assert_ne!(ipv4, Owner::principal("192.0.2.1"));
}

#[tokio::test(start_paused = true)]
async fn empty_receiver_eviction_refuses_reopening_and_ends_its_feed() {
    let store = UploadStore::new().unwrap();
    tokio::time::advance(Duration::from_nanos(1)).await;
    let victim = store.mint().unwrap();
    let owner = Owner::principal("victim");
    drop(store.begin(&victim, &owner).unwrap());
    let mut feed = store.subscribe(&victim, &owner).unwrap();
    assert_eq!(feed.next().await, Some(UploadProgress::Ready));
    tokio::time::advance(Duration::from_millis(1)).await;
    for index in 0..999 {
        store.joined(&Owner::principal(format!("client-{}", index / 31)));
    }
    store.joined(&Owner::principal("replacement"));
    assert!(matches!(feed.next().await, Some(UploadProgress::Error { code, .. }) if code == "invalid"));
    assert_eq!(store.begin(&victim, &owner).err(), Some(UploadRefusal::Invalid));
}

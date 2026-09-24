use graphite_meter_core::wire::{UploadProgress, decode_upload_progress, encode_upload_progress};
use graphite_meter_server::upload::{
    MAX_LIVE_UPLOADS, MAX_UPLOADS_PER_CLIENT, Owner, UPLOAD_RETENTION, UploadError, UploadStore,
};
use std::time::{Duration, Instant};

#[test]
fn tokens_are_stateless_authenticated_and_store_local() {
    let store = UploadStore::new().unwrap();
    let other = UploadStore::new().unwrap();
    for _ in 0..1100 {
        let id = store.mint().unwrap();
        assert_eq!(id.len(), 79);
        assert!(id.starts_with("gmu_"));
    }
    assert_eq!(store.retained(), 0);
    let id = store.mint().unwrap();
    assert_eq!(
        other.begin(&id, &Owner::principal("a")).err(),
        Some(UploadError::Invalid)
    );
    let mut forged = id.clone().into_bytes();
    forged[10] = if forged[10] == b'A' { b'B' } else { b'A' };
    assert_eq!(
        store
            .begin(
                std::str::from_utf8(&forged).unwrap(),
                &Owner::principal("a")
            )
            .err(),
        Some(UploadError::Invalid)
    );
    assert_eq!(
        store.checkpoint(&id, &Owner::principal("a")),
        Err(UploadError::Invalid)
    );
    drop(store.begin(&id, &Owner::principal("a")).unwrap());
    assert_eq!(store.retained(), 1);
}

#[test]
fn delegated_owners_share_capacity_but_not_access() {
    let store = UploadStore::new().unwrap();
    let id = store.mint().unwrap();
    drop(
        store
            .begin(&id, &Owner::delegated("a", "browser:1"))
            .unwrap(),
    );
    assert_eq!(
        store.begin(&id, &Owner::delegated("a", "browser:2")).err(),
        Some(UploadError::OwnerMismatch)
    );
    assert_eq!(
        store.checkpoint(&id, &Owner::delegated("a", "browser:2")),
        Err(UploadError::OwnerMismatch)
    );
    assert_eq!(
        store.finish(&id, &Owner::delegated("a", "browser:2")),
        Err(UploadError::OwnerMismatch)
    );
    assert_eq!(
        store
            .subscribe(&id, &Owner::delegated("a", "browser:2"))
            .err(),
        Some(UploadError::OwnerMismatch)
    );
    for _ in 1..MAX_UPLOADS_PER_CLIENT {
        drop(
            store
                .begin(&store.mint().unwrap(), &Owner::delegated("a", "browser:2"))
                .unwrap(),
        );
    }
    assert_eq!(
        store
            .begin(&store.mint().unwrap(), &Owner::delegated("a", "browser:3"))
            .err(),
        Some(UploadError::ClientFull)
    );
    assert_eq!(
        store
            .begin(&store.mint().unwrap(), &Owner::principal("a"))
            .err(),
        Some(UploadError::ClientFull)
    );
    drop(
        store
            .begin(&store.mint().unwrap(), &Owner::principal("b"))
            .unwrap(),
    );
    store.sweep_at(Instant::now() + UPLOAD_RETENTION + Duration::from_secs(1));
    assert_eq!(store.retained(), 0);
    drop(
        store
            .begin(&store.mint().unwrap(), &Owner::delegated("a", "browser:1"))
            .unwrap(),
    );
}

#[test]
fn global_capacity_is_bounded() {
    let store = UploadStore::new().unwrap();
    for index in 0..MAX_LIVE_UPLOADS {
        drop(
            store
                .begin(&store.mint().unwrap(), &Owner::principal(index.to_string()))
                .unwrap(),
        );
    }
    assert_eq!(
        store
            .begin(&store.mint().unwrap(), &Owner::principal("new-client"))
            .err(),
        Some(UploadError::GlobalFull)
    );
    store.sweep_at(Instant::now() + UPLOAD_RETENTION + Duration::from_secs(1));
    drop(
        store
            .begin(&store.mint().unwrap(), &Owner::principal("new-client"))
            .unwrap(),
    );
}

#[test]
fn concurrent_lanes_credit_each_received_chunk_once() {
    let store = UploadStore::new().unwrap();
    let id = store.mint().unwrap();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let store = &store;
            let id = &id;
            scope.spawn(move || {
                let mut lane = store.begin(id, &Owner::principal("a")).unwrap();
                for _ in 0..500 {
                    lane.record(64);
                }
                assert_eq!(lane.bytes(), 32_000);
            });
        }
    });
    let before = store.checkpoint(&id, &Owner::principal("a")).unwrap();
    assert_eq!(before.bytes, 256_000);
    std::thread::sleep(Duration::from_millis(10));
    let after = store.checkpoint(&id, &Owner::principal("a")).unwrap();
    assert_eq!(after.bytes, before.bytes);
    assert!(
        after.nanos >= before.nanos + 10_000_000,
        "stalls stay in the single aggregate clock"
    );
}

#[tokio::test]
async fn completion_waits_for_lane_drop_and_replays_receiver_totals() {
    let store = UploadStore::new().unwrap();
    let id = store.mint().unwrap();
    let mut subscription = store.subscribe(&id, &Owner::principal("a")).unwrap();
    assert_eq!(subscription.next().await, Some(UploadProgress::Ready));
    let mut first = store.begin(&id, &Owner::principal("a")).unwrap();
    let mut second = store.begin(&id, &Owner::principal("a")).unwrap();
    first.record(100);
    second.record(200);
    store.finish(&id, &Owner::principal("a")).unwrap();
    assert_eq!(
        store.begin(&id, &Owner::principal("a")).err(),
        Some(UploadError::Invalid),
        "a late lane cannot change a terminal receiver total"
    );
    drop(first);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), subscription.next())
            .await
            .is_err()
    );
    second.record(30);
    drop(second);
    let complete = subscription.next().await.unwrap();
    assert!(matches!(complete, UploadProgress::Complete { bytes: 330, nanos } if nanos > 0));
    assert_eq!(
        decode_upload_progress(encode_upload_progress(&complete).unwrap().as_bytes()).unwrap(),
        complete
    );
    assert_eq!(subscription.next().await, None);
    assert_eq!(
        store.begin(&id, &Owner::principal("a")).err(),
        Some(UploadError::Invalid)
    );
    assert_eq!(
        store.checkpoint(&id, &Owner::principal("a")).unwrap().bytes,
        330
    );
    let mut replay = store.subscribe(&id, &Owner::principal("a")).unwrap();
    assert_eq!(replay.next().await, Some(UploadProgress::Ready));
    assert!(matches!(
        replay.next().await,
        Some(UploadProgress::Complete { bytes: 330, .. })
    ));
}

#[tokio::test]
async fn replacing_a_subscriber_and_dropping_stale_claim_preserves_current_claim() {
    let store = UploadStore::new().unwrap();
    let id = store.mint().unwrap();
    let mut first = store.subscribe(&id, &Owner::principal("a")).unwrap();
    first.next().await;
    let mut second = store.subscribe(&id, &Owner::principal("a")).unwrap();
    assert_eq!(first.next().await, None);
    drop(first);
    assert_eq!(second.next().await, Some(UploadProgress::Ready));
    let mut third = store.subscribe(&id, &Owner::principal("a")).unwrap();
    assert_eq!(second.next().await, None);
    drop(second);
    assert_eq!(third.next().await, Some(UploadProgress::Ready));
    store.finish(&id, &Owner::principal("a")).unwrap();
    assert_eq!(
        third.next().await,
        Some(UploadProgress::Complete { bytes: 0, nanos: 0 })
    );
}

#[tokio::test]
async fn retention_preserves_active_lanes_and_expires_observers() {
    let store = UploadStore::new().unwrap();
    let id = store.mint().unwrap();
    let mut lane = store.begin(&id, &Owner::principal("a")).unwrap();
    lane.record(42);
    let mut observer = store.subscribe(&id, &Owner::principal("a")).unwrap();
    observer.next().await;
    let future = Instant::now() + UPLOAD_RETENTION + Duration::from_secs(1);
    store.sweep_at(future);
    assert_eq!(store.retained(), 1);
    assert_eq!(
        store.checkpoint(&id, &Owner::principal("a")).unwrap().bytes,
        42
    );
    drop(lane);
    store.sweep_at(future);
    assert_eq!(store.retained(), 0);
    assert_eq!(observer.next().await, None);
    assert_eq!(
        store.checkpoint(&id, &Owner::principal("a")),
        Err(UploadError::Invalid)
    );
}

#[tokio::test]
async fn cancelled_task_releases_its_lane() {
    let store = UploadStore::new().unwrap();
    let id = store.mint().unwrap();
    let mut lane = store.begin(&id, &Owner::principal("a")).unwrap();
    lane.record(12);
    let task = tokio::spawn(async move {
        let _lane = lane;
        std::future::pending::<()>().await
    });
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    store.finish(&id, &Owner::principal("a")).unwrap();
    let mut subscription = store.subscribe(&id, &Owner::principal("a")).unwrap();
    subscription.next().await;
    assert!(matches!(
        subscription.next().await,
        Some(UploadProgress::Complete { bytes: 12, .. })
    ));
}

// Poll directly with a recording waker: lifecycle must wake an already-suspended
// feed and complete on its next poll, without advancing the progress timer.
struct WakeFlag(std::sync::atomic::AtomicBool);
impl std::task::Wake for WakeFlag {
    fn wake(self: std::sync::Arc<Self>) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn lifecycle_changes_wake_without_waiting_for_progress_tick() {
    use std::{
        future::Future,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        task::{Context, Poll, Waker},
    };
    let store = UploadStore::new().unwrap();
    let id = store.mint().unwrap();
    let mut lane = store.begin(&id, &Owner::principal("a")).unwrap();
    lane.record(25);
    let mut subscription = store.subscribe(&id, &Owner::principal("a")).unwrap();
    assert_eq!(subscription.next().await, Some(UploadProgress::Ready));
    let flag = Arc::new(WakeFlag(AtomicBool::new(false)));
    let waker = Waker::from(flag.clone());
    let mut context = Context::from_waker(&waker);
    let mut pending = Box::pin(subscription.next());
    assert!(pending.as_mut().poll(&mut context).is_pending());
    store.finish(&id, &Owner::principal("a")).unwrap();
    assert!(
        flag.0.swap(false, Ordering::SeqCst),
        "finish must wake the feed"
    );
    assert!(
        pending.as_mut().poll(&mut context).is_pending(),
        "live lane must delay completion"
    );
    drop(lane);
    assert!(
        flag.0.swap(false, Ordering::SeqCst),
        "lane drop must wake the feed"
    );
    assert!(matches!(
        pending.as_mut().poll(&mut context),
        Poll::Ready(Some(UploadProgress::Complete { bytes: 25, .. }))
    ));
    drop(pending);

    let id = store.mint().unwrap();
    let mut first = store.subscribe(&id, &Owner::principal("a")).unwrap();
    first.next().await;
    let mut pending = Box::pin(first.next());
    assert!(pending.as_mut().poll(&mut context).is_pending());
    let mut replacement = store.subscribe(&id, &Owner::principal("a")).unwrap();
    assert!(
        flag.0.swap(false, Ordering::SeqCst),
        "replacement must wake the old feed"
    );
    assert!(matches!(
        pending.as_mut().poll(&mut context),
        Poll::Ready(None)
    ));
    drop(pending);
    replacement.next().await;
    let mut pending = Box::pin(replacement.next());
    assert!(pending.as_mut().poll(&mut context).is_pending());
    store.sweep_at(Instant::now() + UPLOAD_RETENTION + Duration::from_secs(1));
    assert!(
        flag.0.swap(false, Ordering::SeqCst),
        "expiry must wake the feed"
    );
    assert!(matches!(
        pending.as_mut().poll(&mut context),
        Poll::Ready(None)
    ));
}

#[test]
fn owner_fields_cannot_collide_through_delimiters() {
    let store = UploadStore::new().unwrap();
    let subject_with_delimiter = Owner::principal("a\0browser:b");
    let delegated = Owner::delegated("a", "browser:b");
    assert_ne!(subject_with_delimiter, delegated);
    assert_ne!(subject_with_delimiter.budget_key(), delegated.budget_key());
    let first = Owner::delegated("a\0b", "c");
    let second = Owner::delegated("a", "b\0c");
    assert_ne!(first, second);
    let id = store.mint().unwrap();
    drop(store.begin(&id, &first).unwrap());
    assert_eq!(
        store.checkpoint(&id, &second),
        Err(UploadError::OwnerMismatch)
    );
    let principal = Owner::principal("a");
    assert_eq!(principal.budget_key(), delegated.budget_key());
    let id = store.mint().unwrap();
    drop(store.begin(&id, &principal).unwrap());
    assert_eq!(
        store.checkpoint(&id, &delegated),
        Err(UploadError::OwnerMismatch)
    );
    assert_ne!(Owner::delegated("a", ""), principal);
}

#[test]
fn anonymous_owners_canonicalize_ipv4_and_share_ipv6_prefix() {
    let ipv4 = Owner::anonymous("192.0.2.1".parse().unwrap());
    let mapped = Owner::anonymous("::ffff:192.0.2.1".parse().unwrap());
    assert_eq!(ipv4, mapped);
    assert_eq!(ipv4.budget_key(), "192.0.2.1");
    let first = Owner::anonymous("2001:db8:1:2::1".parse().unwrap());
    let second = Owner::anonymous("2001:db8:1:2:ffff::1234".parse().unwrap());
    let other = Owner::anonymous("2001:db8:1:3::1".parse().unwrap());
    assert_eq!(first, second);
    assert_eq!(first.budget_key(), "2001:db8:1:2::/64");
    assert_ne!(first, other);
    assert_ne!(ipv4, Owner::principal("192.0.2.1"));
}

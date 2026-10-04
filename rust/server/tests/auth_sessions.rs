use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_server::auth::{SessionError, SessionStore};
use std::time::Duration;

/// `future`'s output, which must arrive within a second.
async fn settled<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(1), future).await.unwrap()
}

#[test]
fn credentials_are_random_and_metadata_is_stable() {
    let store = SessionStore::new();
    let (token, lease) = store.create("subject", "Display name", "local", None).unwrap();
    let (other, second) = store.create("subject", "Display name", "local", None).unwrap();
    assert_eq!(URL_SAFE_NO_PAD.decode(&token).unwrap().len(), 32);
    assert_eq!(URL_SAFE_NO_PAD.decode(&lease.session().csrf).unwrap().len(), 32);
    assert_eq!(URL_SAFE_NO_PAD.decode(&lease.session().id).unwrap().len(), 16);
    assert_ne!(token, other);
    assert_ne!(lease.session().csrf, second.session().csrf);
    assert_ne!(lease.session().id, second.session().id);
    let found = store.lookup(&token).unwrap();
    assert_eq!(found.session().subject, "subject");
    assert_eq!(found.session().name, "Display name");
    assert_eq!(found.session().provider, "local");
    assert_eq!(found.session().id, lease.session().id);
    assert_eq!(found.session().expires, lease.session().expires);
    assert!(store.lookup("wrong").is_none());
}

#[tokio::test]
async fn rotation_revokes_only_supplied_login_and_notifies_existing_and_late_waiters() {
    let store = SessionStore::new();
    let (old, lease) = store.create("subject", "name", "local", None).unwrap();
    let (sibling, sibling_lease) = store.create("subject", "name", "local", None).unwrap();
    let active = lease.clone();
    let waiting = tokio::spawn(async move { active.ended().await });
    tokio::task::yield_now().await;
    let (replacement, new) = store.create("subject", "name", "local", Some(&old)).unwrap();
    settled(waiting).await.unwrap();
    settled(lease.ended()).await;
    assert!(store.lookup(&old).is_none());
    assert!(store.lookup(&replacement).is_some());
    assert!(store.lookup(&sibling).is_some());
    assert!(new.is_active() && sibling_lease.is_active());
    assert!(!lease.is_active());
    assert!(!store.revoke(&lease));
}

#[tokio::test(start_paused = true)]
async fn oldest_subject_eviction_precedes_global_capacity_and_failed_rotation_preserves_prior() {
    let store = SessionStore::new();
    let (_, oldest) = store.create("subject", "name", "local", None).unwrap();
    tokio::time::advance(Duration::from_millis(1)).await;
    let mut siblings = Vec::new();
    for _ in 1..8 {
        siblings.push(store.create("subject", "name", "local", None).unwrap().1);
    }
    for i in 8..1024 {
        store.create(&format!("subject-{i}"), "name", "local", None).unwrap();
    }
    let (token, replacement) = store.create("subject", "name", "local", None).unwrap();
    assert!(!oldest.is_active());
    assert!(siblings.iter().all(|lease| lease.is_active()));
    assert!(matches!(
        store.create("new subject", "name", "local", Some(&token)),
        Err(SessionError::Capacity)
    ));
    assert!(replacement.is_active());
    assert!(store.lookup(&token).is_some());
}

#[tokio::test]
async fn store_shutdown_revokes_leases_but_dropping_a_clone_does_not() {
    let store = SessionStore::new();
    let clone = store.clone();
    let (_, lease) = store.create("subject", "name", "local", None).unwrap();
    drop(store);
    assert!(lease.is_active());
    drop(clone);
    settled(lease.ended()).await;
    assert!(!lease.is_active());
}

#[tokio::test]
async fn revocation_cannot_be_lost_between_creating_and_polling_waiters() {
    let store = SessionStore::new();
    for _ in 0..100 {
        let (_, lease) = store.create("subject", "name", "local", None).unwrap();
        let ended = lease.ended();
        assert!(store.revoke(&lease));
        settled(ended).await;
    }
}

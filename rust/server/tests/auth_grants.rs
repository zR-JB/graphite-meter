use graphite_meter_server::auth::{AuthLease, GrantError, SessionStore};
use std::time::Duration;

#[test]
fn grant_identity_preserves_parent_budget_but_isolates_browser_uploads() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let cookie = AuthLease::cookie(session.clone());
    let (cli_token, cli) = store.issue_cli_grant(&session).unwrap();
    let (first_token, first) = store
        .issue_browser_grant(&session, "https://client.example")
        .unwrap();
    let (_, second) = store
        .issue_browser_grant(&session, "https://client.example")
        .unwrap();
    assert_eq!(cli.provider(), "cli");
    assert_eq!(first.provider(), "browser");
    assert_eq!(cookie.provider(), "local");
    assert!(cli.is_bearer() && first.is_bearer());
    assert!(!cookie.is_bearer());
    assert_eq!(cli.owner(), cookie.owner());
    assert_ne!(first.owner(), second.owner());
    assert_eq!(first.owner().budget_key(), cookie.owner().budget_key());
    assert_eq!(first.session().id(), second.session().id());
    assert_eq!(
        store.lookup_bearer(&first_token).unwrap().browser_origin(),
        Some("https://client.example")
    );
    assert!(store.lookup_bearer(&cli_token).is_some());
    assert!(store.lookup_bearer(&(cli_token + "=")).is_none());
}

#[test]
fn browser_capacity_preserves_every_existing_grant() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let grants: Vec<_> = (0..8)
        .map(|_| {
            store
                .issue_browser_grant(&session, "https://client.example")
                .unwrap()
        })
        .collect();
    assert!(matches!(
        store.issue_browser_grant(&session, "https://client.example"),
        Err(GrantError::Capacity)
    ));
    for (token, lease) in grants {
        assert!(lease.is_active());
        assert!(store.lookup_bearer(&token).is_some());
    }
}

#[test]
fn cli_capacity_preserves_browser_grants() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let browsers: Vec<_> = (0..8)
        .map(|_| {
            store
                .issue_browser_grant(&session, "https://client.example")
                .unwrap()
        })
        .collect();
    assert!(matches!(
        store.issue_cli_grant(&session),
        Err(GrantError::Capacity)
    ));
    for (token, lease) in browsers {
        assert!(store.lookup_bearer(&token).is_some());
        assert!(lease.is_active());
    }
}

#[test]
fn cli_capacity_evicts_only_the_oldest_native_grant() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let (first, lease) = store.issue_cli_grant(&session).unwrap();
    let (second, _) = store.issue_cli_grant(&session).unwrap();
    let browsers: Vec<_> = (0..6)
        .map(|_| {
            store
                .issue_browser_grant(&session, "https://client.example")
                .unwrap()
        })
        .collect();
    let (new, _) = store.issue_cli_grant(&session).unwrap();
    assert!(store.lookup_bearer(&first).is_none());
    assert!(lease.is_active());
    assert!(store.lookup_bearer(&second).is_some());
    assert!(store.lookup_bearer(&new).is_some());
    for (token, lease) in browsers {
        assert!(store.lookup_bearer(&token).is_some());
        assert!(lease.is_active());
    }
}

#[tokio::test]
async fn cli_revocation_denies_new_requests_but_active_work_remains_parent_bound() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let (token, cli) = store.issue_cli_grant(&session).unwrap();
    assert!(store.revoke_grant(&token));
    assert!(store.lookup_bearer(&token).is_none());
    assert!(cli.is_active());
    store.revoke(&session);
    tokio::time::timeout(Duration::from_secs(1), cli.ended())
        .await
        .unwrap();
    assert!(!cli.is_active());
}

#[tokio::test]
async fn browser_revocation_wakes_existing_waiter_and_preserves_sibling() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let (token, browser) = store
        .issue_browser_grant(&session, "https://client.example")
        .unwrap();
    let (sibling_token, sibling) = store
        .issue_browser_grant(&session, "https://client.example")
        .unwrap();
    let active = browser.clone();
    let waiter = tokio::spawn(async move {
        active.ended().await;
    });
    tokio::task::yield_now().await;
    assert!(store.revoke_grant(&token));
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap();
    assert!(!browser.is_active());
    assert!(sibling.is_active());
    assert!(store.lookup_bearer(&sibling_token).is_some());
    store.revoke(&session);
    tokio::time::timeout(Duration::from_secs(1), sibling.ended())
        .await
        .unwrap();
    assert!(store.lookup_bearer(&sibling_token).is_none());
}

#[test]
fn foreign_and_revoked_sessions_cannot_issue_grants() {
    let store = SessionStore::new();
    let foreign = SessionStore::new();
    let (_, session) = foreign.create("subject", "Name", "local", None).unwrap();
    assert!(matches!(
        store.issue_cli_grant(&session),
        Err(GrantError::NoSession)
    ));
    assert!(matches!(
        store.issue_browser_grant(&session, "https://client.example"),
        Err(GrantError::NoSession)
    ));
    foreign.revoke(&session);
    assert!(matches!(
        foreign.issue_cli_grant(&session),
        Err(GrantError::NoSession)
    ));
}

#[test]
fn browser_audiences_must_be_exact_canonical_https_origins() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    for origin in [
        "http://client.example",
        "https://CLIENT.example",
        "https://client.example:443",
        "https://client.example/",
        "https://user@client.example",
        "https://client.example?",
        "null",
    ] {
        assert!(
            matches!(
                store.issue_browser_grant(&session, origin),
                Err(GrantError::InvalidOrigin)
            ),
            "{origin}"
        );
    }
    assert!(
        store
            .issue_browser_grant(&session, "https://client.example:8443")
            .is_ok()
    );
}

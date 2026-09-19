use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_server::auth::{
    ApprovalError, ApprovalKind, Exchange, ExchangeError, SessionStore, valid_challenge,
};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Barrier};

const AUDIENCE: &str = "https://client.example";
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[test]
fn cli_exchange_requires_approval_is_single_use_and_keeps_parent_identity() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let verifier = "v".repeat(32);
    let challenge = challenge(&verifier);
    let view = store.begin_cli_approval(&session, &challenge).unwrap();
    assert_eq!(view.code, "24NPFPCT");
    assert!(view.attached && view.browser_origin.is_none());
    assert_eq!(
        store.begin_cli_approval(&session, &challenge).unwrap().code,
        view.code
    );
    assert!(matches!(
        store.exchange_cli(&verifier).unwrap(),
        Exchange::Pending
    ));
    assert!(matches!(
        store.approve(&session, &challenge, ApprovalKind::Browser),
        Err(ApprovalError::InvalidApproval)
    ));
    store
        .approve(&session, &challenge, ApprovalKind::Cli)
        .unwrap();
    let Exchange::Issued { token, lease } = store.exchange_cli(&verifier).unwrap() else {
        panic!("not issued")
    };
    assert_eq!(lease.session().id(), session.session().id());
    assert!(store.lookup_bearer(&token).is_some());
    assert!(matches!(
        store.exchange_cli(&verifier).unwrap(),
        Exchange::Pending
    ));
    store.revoke(&session);
    assert!(store.lookup_bearer(&token).is_none());
}

#[test]
fn browser_reservation_attaches_once_preserves_redirect_and_separates_audiences() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let (_, other) = store.create("subject", "Name", "local", None).unwrap();
    let verifier = "v".repeat(32);
    let challenge = challenge(&verifier);
    let pending = store
        .begin_browser_approval(&challenge, AUDIENCE, None)
        .unwrap();
    assert!(!pending.attached);
    assert!(
        store
            .browser_approval_redirect(&challenge)
            .unwrap()
            .contains("client_origin=https%3A%2F%2Fclient.example")
    );
    assert!(matches!(
        store.exchange_browser(&verifier, AUDIENCE).unwrap(),
        Exchange::Pending
    ));
    assert!(matches!(
        store.begin_browser_approval(&challenge, "https://wrong.example", Some(&session)),
        Err(ApprovalError::InvalidApproval)
    ));
    assert!(
        store
            .begin_browser_approval(&challenge, AUDIENCE, Some(&session))
            .unwrap()
            .attached
    );
    assert!(matches!(
        store.begin_browser_approval(&challenge, AUDIENCE, Some(&other)),
        Err(ApprovalError::InvalidApproval)
    ));
    assert!(matches!(
        store.approve(&other, &challenge, ApprovalKind::Browser),
        Err(ApprovalError::InvalidApproval)
    ));
    store
        .approve(&session, &challenge, ApprovalKind::Browser)
        .unwrap();
    assert!(matches!(
        store.exchange_cli(&verifier).unwrap(),
        Exchange::Pending
    ));
    assert!(matches!(
        store
            .exchange_browser(&verifier, "https://wrong.example")
            .unwrap(),
        Exchange::Pending
    ));
    let Exchange::Issued { lease, .. } = store.exchange_browser(&verifier, AUDIENCE).unwrap()
    else {
        panic!("not issued")
    };
    assert_eq!(lease.browser_origin(), Some(AUDIENCE));
    assert!(store.browser_approval_redirect(&challenge).is_none());
}

#[test]
fn browser_capacity_is_reported_without_revoking_or_approving_existing_clients() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let verifier = "v".repeat(32);
    let challenge = challenge(&verifier);
    store
        .begin_browser_approval(&challenge, AUDIENCE, Some(&session))
        .unwrap();
    let grants: Vec<_> = (0..8)
        .map(|_| store.issue_browser_grant(&session, AUDIENCE).unwrap().0)
        .collect();
    assert!(matches!(
        store.begin_browser_approval(&challenge, AUDIENCE, Some(&session)),
        Err(ApprovalError::GrantCapacity)
    ));
    assert!(matches!(
        store.approve(&session, &challenge, ApprovalKind::Browser),
        Err(ApprovalError::GrantCapacity)
    ));
    assert!(matches!(
        store.exchange_browser(&verifier, AUDIENCE),
        Err(ExchangeError::GrantCapacity)
    ));
    assert!(matches!(
        store
            .exchange_browser(&verifier, "https://wrong.example")
            .unwrap(),
        Exchange::Pending
    ));
    for grant in &grants {
        assert!(store.lookup_bearer(grant).is_some());
    }
    store.revoke_grant(&grants[0]);
    assert!(matches!(
        store.exchange_browser(&verifier, AUDIENCE).unwrap(),
        Exchange::Pending
    ));
    store
        .approve(&session, &challenge, ApprovalKind::Browser)
        .unwrap();
    assert!(matches!(
        store.exchange_browser(&verifier, AUDIENCE).unwrap(),
        Exchange::Issued { .. }
    ));
}

#[test]
fn approval_caps_and_revocation_are_bounded_and_reclaimable() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    for i in 0..8 {
        store
            .begin_cli_approval(&session, &challenge(&format!("cli-{i}")))
            .unwrap();
    }
    assert!(matches!(
        store.begin_cli_approval(&session, &challenge("ninth")),
        Err(ApprovalError::Capacity)
    ));
    store.revoke(&session);
    assert!(matches!(
        store.exchange_cli("cli-0").unwrap(),
        Exchange::Pending
    ));
    for i in 0..256 {
        store
            .begin_browser_approval(&challenge(&format!("browser-{i}")), AUDIENCE, None)
            .unwrap();
    }
    assert!(matches!(
        store.begin_browser_approval(&challenge("overflow"), AUDIENCE, None),
        Err(ApprovalError::Capacity)
    ));
    assert!(
        store
            .begin_browser_approval(&challenge("browser-0"), AUDIENCE, None)
            .is_ok()
    );
}

#[test]
fn validation_preserves_cli_and_browser_verifier_differences() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    assert!(!valid_challenge("invalid"));
    assert!(valid_challenge(&URL_SAFE_NO_PAD.encode([255; 32])));
    assert!(!valid_challenge(&(challenge("v") + "=")));
    let challenge = challenge("");
    store.begin_cli_approval(&session, &challenge).unwrap();
    store
        .approve(&session, &challenge, ApprovalKind::Cli)
        .unwrap();
    assert!(matches!(
        store.exchange_cli("").unwrap(),
        Exchange::Issued { .. }
    ));
    assert!(matches!(
        store.exchange_cli(&"x".repeat(129)).unwrap(),
        Exchange::Pending
    ));
    assert!(matches!(
        store.exchange_browser("short", AUDIENCE),
        Err(ExchangeError::InvalidVerifier)
    ));
    assert!(matches!(
        store.exchange_browser(&"x".repeat(32), "http://client.example"),
        Err(ExchangeError::InvalidOrigin)
    ));
    assert!(matches!(
        SessionStore::new().begin_cli_approval(&session, &challenge),
        Err(ApprovalError::NoSession)
    ));
}

#[test]
fn concurrent_exchange_issues_exactly_one_grant() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let verifier = "v".repeat(32);
    let challenge = challenge(&verifier);
    store
        .begin_browser_approval(&challenge, AUDIENCE, Some(&session))
        .unwrap();
    store
        .approve(&session, &challenge, ApprovalKind::Browser)
        .unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let issued = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let barrier = barrier.clone();
                let store = store.clone();
                let verifier = &verifier;
                scope.spawn(move || {
                    barrier.wait();
                    matches!(
                        store.exchange_browser(verifier, AUDIENCE).unwrap(),
                        Exchange::Issued { .. }
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum::<usize>()
    });
    assert_eq!(issued, 1);
}

#[test]
fn logout_and_exchange_are_serializable_and_cannot_leave_a_valid_credential() {
    for _ in 0..40 {
        let store = SessionStore::new();
        let (_, session) = store.create("subject", "Name", "local", None).unwrap();
        let verifier = "v".repeat(32);
        let challenge = challenge(&verifier);
        store.begin_cli_approval(&session, &challenge).unwrap();
        store
            .approve(&session, &challenge, ApprovalKind::Cli)
            .unwrap();
        let barrier = Barrier::new(2);
        let exchange = std::thread::scope(|scope| {
            let exchange = scope.spawn(|| {
                barrier.wait();
                store.exchange_cli(&verifier).unwrap()
            });
            barrier.wait();
            store.revoke(&session);
            exchange.join().unwrap()
        });
        if let Exchange::Issued { token, lease } = exchange {
            assert!(!lease.is_active());
            assert!(store.lookup_bearer(&token).is_none());
        }
        assert!(matches!(
            store.exchange_cli(&verifier).unwrap(),
            Exchange::Pending
        ));
    }
}

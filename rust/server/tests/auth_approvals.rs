use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_core::approval::challenge;
use graphite_meter_server::auth::{
    ApprovalError, ApprovalKind, Challenge, Exchange, ExchangeError, SessionLease, SessionStore, valid_challenge,
};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Barrier},
};

const AUDIENCE: &str = "https://client.example";
const CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));

/// The challenge of `verifier`, validated as the store takes one.
fn valid(verifier: &str) -> Challenge {
    valid_challenge(&challenge(verifier)).unwrap()
}

/// A store with one login, and the challenge, raw and validated, of the 32-byte verifier that follows them.
fn approval() -> (SessionStore, SessionLease, String, Challenge, String) {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let verifier = "v".repeat(32);
    (store, session, challenge(&verifier), valid(&verifier), verifier)
}

fn pending(exchange: Result<Exchange, ExchangeError>) -> bool {
    matches!(exchange, Ok(Exchange::Pending))
}

#[test]
fn cli_exchange_requires_approval_is_single_use_and_keeps_parent_identity() {
    let (store, session, challenge, valid, verifier) = approval();
    let view = store.begin_cli_approval(&session, &valid, CLIENT).unwrap();
    assert_eq!(view.code, "24NPFPCT");
    assert!(view.browser_origin.is_none());
    let again = store.begin_cli_approval(&session, &valid, CLIENT).unwrap();
    assert_eq!(again.code, view.code);
    assert!(pending(store.exchange_cli(&verifier)));
    let wrong_kind = store.approve(&session, &challenge, ApprovalKind::Browser);
    assert_eq!(wrong_kind, Err(ApprovalError::InvalidApproval));
    store.approve(&session, &challenge, ApprovalKind::Cli).unwrap();
    let Exchange::Issued { token, lease } = store.exchange_cli(&verifier).unwrap() else {
        panic!("not issued")
    };
    assert_eq!(lease.session().id, session.session().id);
    assert!(store.lookup_bearer(&token).is_some());
    assert!(pending(store.exchange_cli(&verifier)));
    store.revoke(&session);
    assert!(store.lookup_bearer(&token).is_none());
}

#[test]
fn browser_reservation_attaches_once_preserves_redirect_and_separates_audiences() {
    let (store, session, challenge, valid, verifier) = approval();
    let (_, other) = store.create("subject", "Name", "local", None).unwrap();
    let begin =
        |origin, session: Option<&SessionLease>| store.begin_browser_approval(&valid, origin, session, CLIENT).err();
    assert_eq!(begin(AUDIENCE, None), None);
    let redirect = store.browser_approval_redirect(&challenge).unwrap();
    assert!(redirect.contains("client_origin=https%3A%2F%2Fclient.example"));
    assert!(pending(store.exchange_browser(&verifier, AUDIENCE)));
    assert_eq!(begin("https://wrong.example", Some(&session)), Some(ApprovalError::InvalidApproval));
    assert_eq!(begin(AUDIENCE, Some(&session)), None);
    assert_eq!(begin(AUDIENCE, Some(&other)), Some(ApprovalError::InvalidApproval));
    let foreign = store.approve(&other, &challenge, ApprovalKind::Browser);
    assert_eq!(foreign, Err(ApprovalError::InvalidApproval));
    store.approve(&session, &challenge, ApprovalKind::Browser).unwrap();
    assert!(pending(store.exchange_cli(&verifier)));
    assert!(pending(store.exchange_browser(&verifier, "https://wrong.example")));
    let Exchange::Issued { lease, .. } = store.exchange_browser(&verifier, AUDIENCE).unwrap() else {
        panic!("not issued")
    };
    assert_eq!(lease.browser_origin(), Some(AUDIENCE));
    assert!(store.browser_approval_redirect(&challenge).is_none());
}

#[test]
fn approval_caps_and_revocation_are_bounded_and_reclaimable() {
    let (store, session, ..) = approval();
    let cli = |session: &SessionLease, verifier: &str, client| {
        store.begin_cli_approval(session, &valid(verifier), client).err()
    };
    let browser = |verifier: &str, client| {
        store
            .begin_browser_approval(&valid(verifier), AUDIENCE, None, client)
            .err()
    };
    for i in 0..8 {
        assert_eq!(cli(&session, &format!("cli-{i}"), CLIENT), None);
    }
    assert_eq!(cli(&session, "ninth", CLIENT), Some(ApprovalError::Capacity));
    store.revoke(&session);
    assert!(pending(store.exchange_cli("cli-0")));
    let client = |network, i: usize| IpAddr::V4(Ipv4Addr::new(network, 0, 2, (i / 8) as u8));
    for i in 0..128 {
        assert_eq!(browser(&format!("browser-{i}"), client(192, i)), None);
    }
    assert_eq!(browser("anonymous", client(198, 0)), Some(ApprovalError::Capacity));
    for i in 0..128 {
        let (_, session) = store.create(&format!("subject-{i}"), "Name", "local", None).unwrap();
        assert_eq!(cli(&session, &format!("signed-in-{i}"), client(203, i)), None);
    }
    let (_, session) = store.create("last", "Name", "local", None).unwrap();
    assert_eq!(cli(&session, "overflow", client(198, 0)), Some(ApprovalError::Capacity));
    assert_eq!(browser("browser-0", CLIENT), None);
}

#[test]
fn validation_preserves_cli_and_browser_verifier_differences() {
    let (store, session, ..) = approval();
    assert!(valid_challenge("invalid").is_none());
    assert!(valid_challenge(&URL_SAFE_NO_PAD.encode([255; 32])).is_some());
    assert!(valid_challenge(&(challenge("v") + "=")).is_none());
    let challenge = challenge("");
    let valid = valid("");
    store.begin_cli_approval(&session, &valid, CLIENT).unwrap();
    store.approve(&session, &challenge, ApprovalKind::Cli).unwrap();
    assert!(matches!(store.exchange_cli("").unwrap(), Exchange::Issued { .. }));
    assert!(pending(store.exchange_cli(&"x".repeat(129))));
    assert!(matches!(store.exchange_browser("short", AUDIENCE), Err(ExchangeError::InvalidVerifier)));
    let foreign = SessionStore::new().begin_cli_approval(&session, &valid, CLIENT);
    assert_eq!(foreign.err(), Some(ApprovalError::NoSession));
}

#[test]
fn concurrent_exchange_issues_exactly_one_grant() {
    let (store, session, challenge, valid, verifier) = approval();
    let begun = store.begin_browser_approval(&valid, AUDIENCE, Some(&session), CLIENT);
    assert!(begun.is_ok());
    store.approve(&session, &challenge, ApprovalKind::Browser).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let issued = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let barrier = barrier.clone();
                let store = store.clone();
                let verifier = &verifier;
                scope.spawn(move || {
                    barrier.wait();
                    matches!(store.exchange_browser(verifier, AUDIENCE).unwrap(), Exchange::Issued { .. })
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
        let (store, session, challenge, valid, verifier) = approval();
        store.begin_cli_approval(&session, &valid, CLIENT).unwrap();
        store.approve(&session, &challenge, ApprovalKind::Cli).unwrap();
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
        assert!(pending(store.exchange_cli(&verifier)));
    }
}

#[tokio::test(start_paused = true)]
async fn pending_approvals_share_client_subnet_capacity_and_expire() {
    let (store, session, ..) = approval();
    let first = "2001:db8::1".parse().unwrap();
    let same = "2001:db8::2".parse().unwrap();
    let other = "2001:db8:0:1::1".parse().unwrap();
    let browser = |verifier: &str, session: Option<&SessionLease>, client| {
        store
            .begin_browser_approval(&valid(verifier), AUDIENCE, session, client)
            .err()
    };
    for i in 0..8 {
        assert_eq!(browser(&format!("pending-{i}"), None, first), None);
    }
    assert_eq!(browser("pending-0", Some(&session), same), None);
    assert_eq!(browser("ninth", None, same), Some(ApprovalError::Capacity));
    let cli = store.begin_cli_approval(&session, &valid("cli"), same);
    assert_eq!(cli.err(), Some(ApprovalError::Capacity));
    assert_eq!(browser("other", None, other), None);
    tokio::time::advance(std::time::Duration::from_secs(120)).await;
    store.begin_cli_approval(&session, &valid("cli"), same).unwrap();
    assert_eq!(browser("ninth", None, first), None);
}

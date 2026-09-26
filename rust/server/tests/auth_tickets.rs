use graphite_meter_server::auth::{AuthLease, SessionStore, SocketKind, TicketError};
use std::time::{Duration, SystemTime};

const PUBLIC: &str = "https://meter.example";
const TARGET: &str = "https://meter.example:8443/wt/ping";
const AUDIENCE: &str = "https://client.example";

#[test]
fn ticket_retains_cookie_identity_but_becomes_nonambient_and_single_use() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let cookie = AuthLease::cookie(session);
    let before = SystemTime::now();
    let ticket = store
        .mint_ticket(&cookie, PUBLIC, TARGET, PUBLIC, SocketKind::WebTransport)
        .unwrap();
    assert!(ticket.token.starts_with("gmw_"));
    assert_eq!(ticket.token.len(), 47);
    assert!(ticket.expires >= before + Duration::from_secs(29));
    assert!(ticket.expires <= SystemTime::now() + Duration::from_secs(30));
    let consumed = store.consume_ticket(&ticket.token, TARGET, PUBLIC).unwrap();
    assert!(consumed.is_bearer());
    assert_eq!(consumed.provider(), "local");
    assert_eq!(consumed.owner(), cookie.owner());
    assert!(
        store
            .consume_ticket(&ticket.token, TARGET, PUBLIC)
            .is_none()
    );
    assert!(matches!(
        store.mint_ticket(&consumed, PUBLIC, TARGET, PUBLIC, SocketKind::WebTransport),
        Err(TicketError::NoSession)
    ));
}

#[test]
fn failed_binding_burns_ticket_and_combined_capacity_is_preserved() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let cookie = AuthLease::cookie(session);
    for (target, origin) in [
        ("https://meter.example/wt/ping", PUBLIC),
        ("https://meter.example:8443/wt/upload", PUBLIC),
        (TARGET, AUDIENCE),
    ] {
        let ticket = store
            .mint_ticket(&cookie, PUBLIC, TARGET, PUBLIC, SocketKind::WebTransport)
            .unwrap();
        assert!(
            store
                .consume_ticket(&ticket.token, target, origin)
                .is_none()
        );
        assert!(
            store
                .consume_ticket(&ticket.token, TARGET, PUBLIC)
                .is_none()
        );
    }
    let tickets: Vec<_> = (0..8)
        .map(|i| {
            let (target, kind) = if i % 2 == 0 {
                (TARGET, SocketKind::WebTransport)
            } else {
                ("https://meter.example/ws/ping", SocketKind::WebSocket)
            };
            (
                store
                    .mint_ticket(&cookie, PUBLIC, target, PUBLIC, kind)
                    .unwrap(),
                target,
            )
        })
        .collect();
    assert!(matches!(
        store.mint_ticket(&cookie, PUBLIC, TARGET, PUBLIC, SocketKind::WebTransport),
        Err(TicketError::Capacity)
    ));
    for (ticket, target) in tickets {
        assert!(
            store
                .consume_ticket(&ticket.token, target, PUBLIC)
                .is_some()
        );
    }
    assert!(
        store
            .mint_ticket(&cookie, PUBLIC, TARGET, PUBLIC, SocketKind::WebTransport)
            .is_ok()
    );
}

#[tokio::test]
async fn tickets_preserve_browser_child_revocation_and_owner() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let (grant, browser) = store.issue_browser_grant(&session, AUDIENCE).unwrap();
    let (_, sibling) = store.issue_browser_grant(&session, AUDIENCE).unwrap();
    let ticket = store
        .mint_ticket(&browser, PUBLIC, TARGET, AUDIENCE, SocketKind::WebTransport)
        .unwrap();
    let unused = store
        .mint_ticket(&browser, PUBLIC, TARGET, AUDIENCE, SocketKind::WebTransport)
        .unwrap();
    let active = store
        .consume_ticket(&ticket.token, TARGET, AUDIENCE)
        .unwrap();
    assert_eq!(active.owner(), browser.owner());
    assert_ne!(active.owner(), sibling.owner());
    assert_eq!(active.browser_origin(), Some(AUDIENCE));
    store.revoke_grant(&grant);
    tokio::time::timeout(Duration::from_secs(1), active.ended())
        .await
        .unwrap();
    assert!(
        store
            .consume_ticket(&unused.token, TARGET, AUDIENCE)
            .is_none()
    );
    assert!(sibling.is_active());
    assert!(matches!(
        store.mint_ticket(&browser, PUBLIC, TARGET, AUDIENCE, SocketKind::WebTransport),
        Err(TicketError::NoSession)
    ));
}

#[test]
fn ticket_mint_rejects_cli_foreign_sessions_and_invalid_targets() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let (_, cli) = store.issue_cli_grant(&session).unwrap();
    assert!(matches!(
        store.mint_ticket(&cli, PUBLIC, TARGET, "", SocketKind::WebTransport),
        Err(TicketError::NoSession)
    ));
    let cookie = AuthLease::cookie(session.clone());
    assert!(matches!(
        SessionStore::new().mint_ticket(&cookie, PUBLIC, TARGET, PUBLIC, SocketKind::WebTransport),
        Err(TicketError::NoSession)
    ));
    for target in [
        "http://meter.example/wt/ping",
        "https://other.example/wt/ping",
        "https://user@meter.example/wt/ping",
        "https://meter.example/wt/ping?",
        "https://meter.example/wt/ping?x=1",
        "https://meter.example/wt/ping#x",
        "https://meter.example/ws/ping",
        "https://meter.example/preflight",
        "https://meter.example/other/../wt/ping",
    ] {
        assert!(
            matches!(
                store.mint_ticket(&cookie, PUBLIC, target, PUBLIC, SocketKind::WebTransport),
                Err(TicketError::InvalidTarget)
            ),
            "{target}"
        );
    }
    let ticket = store
        .mint_ticket(
            &cookie,
            PUBLIC,
            "https://METER.example:443/wt/%70ing",
            PUBLIC,
            SocketKind::WebTransport,
        )
        .unwrap();
    assert!(
        store
            .consume_ticket(&ticket.token, "https://meter.example/wt/ping", PUBLIC)
            .is_some()
    );
    let ticket = store
        .mint_ticket(&cookie, PUBLIC, TARGET, PUBLIC, SocketKind::WebTransport)
        .unwrap();
    store.revoke(&session);
    assert!(
        store
            .consume_ticket(&ticket.token, TARGET, PUBLIC)
            .is_none()
    );
}

use graphite_meter_core::route::Kind;
use graphite_meter_server::auth::{AuthLease, SessionStore, Ticket, TicketError};
use std::time::{Duration, SystemTime};

const PUBLIC: &str = "https://meter.example";
const TARGET: &str = "https://meter.example:8443/wt/ping";
const AUDIENCE: &str = "https://client.example";

/// A WebTransport ticket for `target` that `lease` mints from the public origin.
fn mint(store: &SessionStore, lease: &AuthLease, target: &str) -> Result<Ticket, TicketError> {
    store.mint_ticket(lease, PUBLIC, target, PUBLIC, Kind::WebTransport)
}

#[test]
fn ticket_retains_cookie_identity_but_becomes_nonambient_and_single_use() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let cookie = AuthLease::cookie(session);
    let before = SystemTime::now();
    let ticket = mint(&store, &cookie, TARGET).unwrap();
    assert!(ticket.token.starts_with("gmw_"));
    assert_eq!(ticket.token.len(), 47);
    assert!(ticket.expires >= before + Duration::from_secs(29));
    assert!(ticket.expires <= SystemTime::now() + Duration::from_secs(30));
    let consumed = store.consume_ticket(&ticket.token, TARGET, PUBLIC).unwrap();
    assert!(consumed.is_bearer());
    assert_eq!(consumed.provider(), "local");
    assert_eq!(consumed.owner(), cookie.owner());
    assert!(store.consume_ticket(&ticket.token, TARGET, PUBLIC).is_none());
    assert!(matches!(mint(&store, &consumed, TARGET), Err(TicketError::NoSession)));
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
        let ticket = mint(&store, &cookie, TARGET).unwrap();
        assert!(store.consume_ticket(&ticket.token, target, origin).is_none());
        assert!(store.consume_ticket(&ticket.token, TARGET, PUBLIC).is_none());
    }
    let tickets: Vec<_> = (0..8)
        .map(|i| {
            let (target, kind) = if i % 2 == 0 {
                (TARGET, Kind::WebTransport)
            } else {
                ("https://meter.example/ws/ping", Kind::WebSocket)
            };
            let ticket = store.mint_ticket(&cookie, PUBLIC, target, PUBLIC, kind).unwrap();
            (ticket, target)
        })
        .collect();
    assert!(matches!(mint(&store, &cookie, TARGET), Err(TicketError::Capacity)));
    for (ticket, target) in tickets {
        assert!(store.consume_ticket(&ticket.token, target, PUBLIC).is_some());
    }
    assert!(mint(&store, &cookie, TARGET).is_ok());
}

#[test]
fn ticket_mint_rejects_cli_foreign_sessions_and_invalid_targets() {
    let store = SessionStore::new();
    let (_, session) = store.create("subject", "Name", "local", None).unwrap();
    let cookie = AuthLease::cookie(session.clone());
    let foreign = mint(&SessionStore::new(), &cookie, TARGET);
    assert!(matches!(foreign, Err(TicketError::NoSession)));
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
        let refused = mint(&store, &cookie, target);
        assert!(matches!(refused, Err(TicketError::InvalidTarget)), "{target}");
    }
    let ticket = mint(&store, &cookie, "https://METER.example:443/wt/%70ing").unwrap();
    let canonical = "https://meter.example/wt/ping";
    assert!(store.consume_ticket(&ticket.token, canonical, PUBLIC).is_some());
    let ticket = mint(&store, &cookie, TARGET).unwrap();
    store.revoke(&session);
    assert!(store.consume_ticket(&ticket.token, TARGET, PUBLIC).is_none());
}

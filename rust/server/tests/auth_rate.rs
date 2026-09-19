use graphite_meter_server::auth::rate::{AttemptLimiter, Budget};
use std::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

fn address(number: u32) -> IpAddr {
    Ipv4Addr::from(number).into()
}

#[tokio::test(start_paused = true)]
async fn address_limits_are_separate_and_expire_exactly_at_sixty_seconds() {
    let limiter = AttemptLimiter::new();
    let client = address(1);
    for (budget, limit) in [
        (Budget::Password, 5),
        (Budget::OidcExchange, 10),
        (Budget::BrowserApproval, 10),
    ] {
        for _ in 0..limit {
            assert!(limiter.allow(budget, client));
        }
        assert!(!limiter.allow(budget, client));
    }
    tokio::time::advance(Duration::from_secs(60) - Duration::from_nanos(1)).await;
    for budget in [
        Budget::Password,
        Budget::OidcExchange,
        Budget::BrowserApproval,
    ] {
        assert!(!limiter.allow(budget, client));
    }
    tokio::time::advance(Duration::from_nanos(1)).await;
    for budget in [
        Budget::Password,
        Budget::OidcExchange,
        Budget::BrowserApproval,
    ] {
        assert!(limiter.allow(budget, client));
    }
}

#[tokio::test(start_paused = true)]
async fn global_refusals_do_not_spend_address_budget() {
    let limiter = AttemptLimiter::new();
    for id in 1..=12 {
        for _ in 0..5 {
            assert!(limiter.allow(Budget::Password, address(id)));
        }
    }
    tokio::time::advance(Duration::from_secs(30)).await;
    let refused = address(100);
    for _ in 0..20 {
        assert!(!limiter.allow(Budget::Password, refused));
    }
    assert!(limiter.allow(Budget::OidcExchange, refused));
    assert!(limiter.allow(Budget::BrowserApproval, refused));
    tokio::time::advance(Duration::from_secs(30)).await;
    for _ in 0..5 {
        assert!(limiter.allow(Budget::Password, refused));
    }
    assert!(!limiter.allow(Budget::Password, refused));
}

#[tokio::test(start_paused = true)]
async fn rolling_window_expires_individual_attempts() {
    let limiter = AttemptLimiter::new();
    let client = address(1);
    assert!(limiter.allow(Budget::Password, client));
    tokio::time::advance(Duration::from_secs(30)).await;
    for _ in 0..4 {
        assert!(limiter.allow(Budget::Password, client));
    }
    tokio::time::advance(Duration::from_secs(30)).await;
    assert!(limiter.allow(Budget::Password, client));
    assert!(!limiter.allow(Budget::Password, client));
}

#[tokio::test(start_paused = true)]
async fn address_keys_unmap_ipv4_and_group_ipv6_by_64_bit_prefix() {
    let limiter = AttemptLimiter::new();
    for _ in 0..5 {
        assert!(limiter.allow(Budget::Password, "192.0.2.1".parse().unwrap()));
    }
    assert!(!limiter.allow(Budget::Password, "::ffff:192.0.2.1".parse().unwrap()));
    for suffix in 1..=10 {
        let client = format!("2001:db8:1:2::{suffix:x}").parse().unwrap();
        assert!(limiter.allow(Budget::OidcExchange, client));
    }
    assert!(!limiter.allow(
        Budget::OidcExchange,
        "2001:db8:1:2:ffff::1".parse().unwrap()
    ));
    assert!(limiter.allow(Budget::OidcExchange, "2001:db8:1:3::1".parse().unwrap()));
}

#[tokio::test(start_paused = true)]
async fn each_address_map_is_bounded_and_reclaims_only_expired_keys() {
    let limiter = AttemptLimiter::new();
    for budget in [Budget::OidcExchange, Budget::BrowserApproval] {
        for id in 1..=2048 {
            assert!(limiter.allow(budget, address(id)));
        }
        assert!(!limiter.allow(budget, address(2049)));
        assert!(limiter.allow(budget, address(1)));
    }
    tokio::time::advance(Duration::from_secs(59)).await;
    for budget in [Budget::OidcExchange, Budget::BrowserApproval] {
        assert!(!limiter.allow(budget, address(2049)));
        assert!(limiter.allow(budget, address(1)));
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    for budget in [Budget::OidcExchange, Budget::BrowserApproval] {
        assert!(limiter.allow(budget, address(2049)));
        // The live key survives reclamation with its one recent attempt.
        for _ in 0..9 {
            assert!(limiter.allow(budget, address(1)));
        }
        assert!(!limiter.allow(budget, address(1)));
    }
}

#[test]
fn simultaneous_password_attempts_cannot_overspend_global_budget() {
    let limiter = AttemptLimiter::new();
    let barrier = std::sync::Barrier::new(80);
    let accepted = std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for id in 1..=80 {
            let limiter = &limiter;
            let barrier = &barrier;
            workers.push(scope.spawn(move || {
                barrier.wait();
                limiter.allow(Budget::Password, address(id))
            }));
        }
        workers
            .into_iter()
            .map(|worker| usize::from(worker.join().unwrap()))
            .sum::<usize>()
    });
    assert_eq!(accepted, 60);
}

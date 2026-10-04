use graphite_meter_server::connections::Connections;

#[test]
fn ipv6_subnets_and_mapped_ipv4_share_the_expected_budget() {
    let capacity = Connections::new(4, 1, Vec::new());
    let acquire = |peer: &str| capacity.acquire(peer.parse().unwrap(), false);
    let first = acquire("[2001:db8:1::1]:1000").unwrap();
    assert!(acquire("[2001:db8:1::2]:2000").is_none());
    let other = acquire("[2001:db8:2::1]:1000").unwrap();
    let mapped = acquire("[::ffff:198.51.100.1]:1000").unwrap();
    assert!(acquire("198.51.100.1:2000").is_none());
    assert_eq!(capacity.stats().active, 3);
    drop((first, other, mapped));
    assert_eq!(capacity.stats().active, 0);
    assert_eq!(capacity.stats().peak, 3);
    assert_eq!(capacity.stats().rejected_client, 2);
}

#[tokio::test]
async fn trusted_proxy_exemption_keeps_global_limit_and_cancellation_releases_it() {
    let capacity = Connections::new(2, 1, vec!["10.0.0.0/8".parse().unwrap()]);
    let peer = "10.0.0.2:1234".parse().unwrap();
    let first = capacity.acquire(peer, false).unwrap();
    let second = capacity.acquire(peer, false).unwrap();
    assert!(capacity.acquire(peer, false).is_none());
    let task = tokio::spawn(async move {
        let _permit = second;
        std::future::pending::<()>().await;
    });
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(capacity.stats().active, 1);
    assert!(capacity.acquire(peer, false).is_some());
    drop(first);
    assert_eq!(capacity.stats().active, 0);
    assert_eq!(capacity.stats().rejected_global, 1);
}

#[test]
fn buffered_connections_share_wider_ipv6_budgets_and_release_them() {
    let capacity = Connections::new(128, 64, Vec::new());
    let mut permits = Vec::new();
    for subnet in 0..2 {
        for port in 1..=8 {
            let peer = format!("[2001:db8:1:{subnet:x}::1]:{port}").parse().unwrap();
            permits.push(capacity.acquire(peer, true).unwrap());
        }
    }
    let acquire = |peer: &str| capacity.acquire(peer.parse().unwrap(), true);
    assert!(acquire("[2001:db8:1:2::1]:9").is_none());
    assert!(acquire("[2001:db8:2::1]:9").is_some());
    drop(permits);
    assert!(acquire("[2001:db8:1:2::1]:9").is_some());
}

use graphite_meter_server::connections::{Connections, Refusal};

#[test]
fn ipv6_subnets_and_mapped_ipv4_share_the_expected_budget() {
    let capacity = Connections::new(4, 1, Vec::new());
    let first = capacity
        .acquire("[2001:db8:1::1]:1000".parse().unwrap(), false)
        .unwrap();
    assert_eq!(
        capacity.acquire("[2001:db8:1::2]:2000".parse().unwrap(), false).err(),
        Some(Refusal::ClientFull)
    );
    let other = capacity
        .acquire("[2001:db8:2::1]:1000".parse().unwrap(), false)
        .unwrap();
    let mapped = capacity
        .acquire("[::ffff:198.51.100.1]:1000".parse().unwrap(), false)
        .unwrap();
    assert_eq!(
        capacity.acquire("198.51.100.1:2000".parse().unwrap(), false).err(),
        Some(Refusal::ClientFull)
    );
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
    assert_eq!(capacity.acquire(peer, false).err(), Some(Refusal::GlobalFull));
    let task = tokio::spawn(async move {
        let _permit = second;
        std::future::pending::<()>().await;
    });
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(capacity.stats().active, 1);
    assert!(capacity.acquire(peer, false).is_ok());
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
    assert_eq!(
        capacity.acquire("[2001:db8:1:2::1]:9".parse().unwrap(), true).err(),
        Some(Refusal::ClientFull)
    );
    assert!(capacity.acquire("[2001:db8:2::1]:9".parse().unwrap(), true).is_ok());
    drop(permits);
    assert!(capacity.acquire("[2001:db8:1:2::1]:9".parse().unwrap(), true).is_ok());
}

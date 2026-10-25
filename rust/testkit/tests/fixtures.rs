//! The UDP relay keeps relaying after ICMP errors from its target, and its bottleneck paces and drops.
use graphite_meter_testkit::{Bottleneck, Error, Link};
use std::time::Duration;
use tokio::{
    net::UdpSocket,
    time::{Instant, timeout},
};

#[tokio::test]
async fn udp_link_outlasts_a_target_that_is_not_bound_yet() -> Result<(), Error> {
    let target = UdpSocket::bind("127.0.0.1:0").await?.local_addr()?;
    let link = Link::udp(target, Duration::ZERO, None).await?;
    let client = UdpSocket::bind("127.0.0.1:0").await?;
    client.connect(link.address).await?;
    // The relay's send draws ICMP port unreachable, which its next receive reports.
    client.send(b"\x40lost").await?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let echo = UdpSocket::bind(target).await?;
    tokio::spawn(async move {
        let mut buffer = [0; 1500];
        while let Ok((count, from)) = echo.recv_from(&mut buffer).await {
            let _ = echo.send_to(&buffer[..count], from).await;
        }
    });
    let mut reply = [0; 16];
    client.send(b"\x40short").await?;
    let count = timeout(Duration::from_secs(5), client.recv(&mut reply)).await??;
    assert_eq!(&reply[..count], b"\x40short");
    Ok(())
}

#[tokio::test]
async fn a_bottleneck_paces_a_burst_and_drops_what_its_queue_cannot_hold() -> Result<(), Error> {
    let target = UdpSocket::bind("127.0.0.1:0").await?;
    let bottleneck = Bottleneck {
        bits_per_second: 2_000_000,
        queue: Duration::from_millis(50),
    };
    let link = Link::udp(target.local_addr()?, Duration::ZERO, Some(bottleneck)).await?;
    let client = UdpSocket::bind("127.0.0.1:0").await?;
    client.connect(link.address).await?;
    let burst = Instant::now();
    for _ in 0..100 {
        client.send(&[0; 1200]).await?;
    }
    let (mut passed, mut last) = (0, burst);
    let mut buffer = [0; 1500];
    while let Ok(received) = timeout(Duration::from_millis(500), target.recv(&mut buffer)).await {
        received?;
        (passed, last) = (passed + 1, Instant::now());
    }
    assert!((8..=50).contains(&passed), "{passed} packets passed");
    let paced = Duration::from_nanos(1200 * 8 * 1_000_000_000 / 2_000_000) * passed;
    assert!(
        last - burst >= paced,
        "{passed} packets passed in {:?}",
        last - burst
    );
    Ok(())
}

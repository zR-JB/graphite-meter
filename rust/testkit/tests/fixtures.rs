//! The UDP relay keeps relaying after ICMP errors from its target.
use graphite_meter_testkit::{Error, Link};
use std::time::Duration;
use tokio::{net::UdpSocket, time::timeout};

#[tokio::test]
async fn udp_link_outlasts_a_target_that_is_not_bound_yet() -> Result<(), Error> {
    let target = UdpSocket::bind("127.0.0.1:0").await?.local_addr()?;
    let link = Link::udp(target, Duration::ZERO).await?;
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

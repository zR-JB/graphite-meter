//! What transport tests rely on: the relay's delay and faults.
use graphite_meter_testkit::{Error, Fault, Link};
use std::{io, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    time::{Instant, timeout},
};

#[tokio::test]
async fn tcp_link_delays_stalls_and_resets() -> Result<(), Error> {
    let echo = TcpListener::bind("127.0.0.1:0").await?;
    let target = echo.local_addr()?;
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = stream.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    let link = Link::tcp(target, Duration::from_millis(20)).await?;
    let mut stream = TcpStream::connect(link.address).await?;
    let mut reply = [0; 4];
    let started = Instant::now();
    stream.write_all(b"ping").await?;
    stream.read_exact(&mut reply).await?;
    assert!(started.elapsed() >= Duration::from_millis(40), "{:?}", started.elapsed());
    link.inject(Fault::Stall);
    stream.write_all(b"ping").await?;
    let stalled = timeout(Duration::from_millis(200), stream.read(&mut reply)).await;
    assert!(stalled.is_err(), "nothing passes a stall");
    link.inject(Fault::Reset);
    let reset = timeout(Duration::from_secs(5), stream.read(&mut reply)).await?;
    assert_eq!(reset.map_err(|error| error.kind()), Err(io::ErrorKind::ConnectionReset));
    Ok(())
}

#[tokio::test]
async fn udp_link_delays_counts_retries_and_stalls() -> Result<(), Error> {
    let echo = UdpSocket::bind("127.0.0.1:0").await?;
    let target = echo.local_addr()?;
    tokio::spawn(async move {
        let mut buffer = [0; 1500];
        while let Ok((count, from)) = echo.recv_from(&mut buffer).await {
            let _ = echo.send_to(&buffer[..count], from).await;
        }
    });
    let link = Link::udp(target, Duration::from_millis(20)).await?;
    let client = UdpSocket::bind("127.0.0.1:0").await?;
    client.connect(link.address).await?;
    let mut reply = [0; 16];
    for (packet, retries) in [(&b"\x40short"[..], 0), (b"\xf0retry", 1)] {
        let started = Instant::now();
        client.send(packet).await?;
        client.recv(&mut reply).await?;
        assert!(started.elapsed() >= Duration::from_millis(40), "{:?}", started.elapsed());
        assert_eq!(link.retries(), retries);
    }
    link.inject(Fault::Stall);
    client.send(b"\x40short").await?;
    let stalled = timeout(Duration::from_millis(200), client.recv(&mut reply)).await;
    assert!(stalled.is_err(), "nothing passes a stall");
    Ok(())
}

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

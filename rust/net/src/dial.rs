//! TCP dialing by Go's algorithm: the resolver's first family first, the other after a delay, a timeout and
//! keep-alive.
use graphite_meter_proto::origin::Host;
use std::{io, net::SocketAddr, pin::pin, time::Duration};
use tokio::{net::TcpStream, time::Instant};

/// A second under Go's 10 s, so the client's 10 s control deadline never ends a dial.
const TIMEOUT: Duration = Duration::from_secs(9);
/// Go's wait before the other address family joins.
const FALLBACK_DELAY: Duration = Duration::from_millis(300);
/// Go's least share of the remaining time for each address in a family.
const MIN_SHARE: Duration = Duration::from_secs(2);
/// Go's keep-alive idle time, probe interval and probe count.
const KEEP_ALIVE_IDLE: Duration = Duration::from_secs(30);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEP_ALIVE_PROBES: u32 = 9;

/// The addresses `host` names, without a lookup for an IP address.
pub async fn resolve(host: &Host, port: u16) -> io::Result<Vec<SocketAddr>> {
    match host {
        Host::Ip(ip) => Ok(vec![SocketAddr::new(*ip, port)]),
        Host::Name(name) => Ok(tokio::net::lookup_host((name.as_str(), port)).await?.collect()),
    }
}

/// A connection to an address of `host` within the timeout, which covers the lookup.
pub(crate) async fn dial(host: &Host, port: u16) -> io::Result<TcpStream> {
    let deadline = Instant::now() + TIMEOUT;
    let dial = async {
        let (primaries, fallbacks) = families(resolve(host, port).await?);
        let stream = parallel(&primaries, &fallbacks, deadline).await?;
        stream.set_nodelay(true)?;
        keep_alive(&stream);
        Ok(stream)
    };
    tokio::time::timeout_at(deadline, dial)
        .await
        .unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()))
}

/// The addresses of the first address's family, then the others, each in the resolver's order.
fn families(addresses: Vec<SocketAddr>) -> (Vec<SocketAddr>, Vec<SocketAddr>) {
    let first_v6 = addresses.first().is_some_and(SocketAddr::is_ipv6);
    addresses.into_iter().partition(|address| address.is_ipv6() == first_v6)
}

/// The primaries in turn, raced by the fallbacks from the delay or the primaries' failure; with both failed, the
/// primaries' error.
async fn parallel(primaries: &[SocketAddr], fallbacks: &[SocketAddr], deadline: Instant) -> io::Result<TcpStream> {
    if fallbacks.is_empty() {
        return serial(primaries, deadline).await;
    }
    let mut primary = pin!(serial(primaries, deadline));
    let mut fallback = pin!(serial(fallbacks, deadline));
    let primary_error = tokio::select! {
        result = &mut primary => match result {
            Ok(stream) => return Ok(stream),
            Err(error) => error,
        },
        result = async {
            tokio::time::sleep(FALLBACK_DELAY).await;
            (&mut fallback).await
        } => match result {
            Ok(stream) => return Ok(stream),
            Err(_) => return primary.await,
        },
    };
    fallback.await.map_err(|_| primary_error)
}

/// Each address in turn, each given its share of the time left; the first address's error if none connects.
async fn serial(addresses: &[SocketAddr], deadline: Instant) -> io::Result<TcpStream> {
    let mut first = None;
    for (index, address) in addresses.iter().enumerate() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            first.get_or_insert(io::ErrorKind::TimedOut.into());
            break;
        }
        let attempt = tokio::time::timeout(share(left, addresses.len() - index), TcpStream::connect(address));
        match attempt.await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => first.get_or_insert(error),
            Err(_) => first.get_or_insert(io::ErrorKind::TimedOut.into()),
        };
    }
    Err(first.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address resolved")))
}

/// Go's partialDeadline: an equal part of `left` for each of `addresses`, but at least `MIN_SHARE` while it lasts.
fn share(left: Duration, addresses: usize) -> Duration {
    let equal = left / u32::try_from(addresses).unwrap_or(u32::MAX);
    equal.max(MIN_SHARE.min(left))
}

/// Probes an idle peer after 30 s and every 15 s after, nine times; a socket that refuses the options still
/// connects.
fn keep_alive(stream: &TcpStream) {
    let probes = socket2::TcpKeepalive::new().with_time(KEEP_ALIVE_IDLE);
    #[cfg(any(target_os = "linux", target_vendor = "apple", windows))]
    let probes = probes
        .with_interval(KEEP_ALIVE_INTERVAL)
        .with_retries(KEEP_ALIVE_PROBES);
    let _ = socket2::SockRef::from(stream).set_tcp_keepalive(&probes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::{net::TcpListener, time::Instant};

    async fn listener() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        (listener, address)
    }

    /// An address whose full accept queue drops new handshakes, so a connect to it hangs.
    #[cfg(target_os = "linux")]
    fn silent() -> (socket2::Socket, std::net::TcpStream) {
        let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
        socket.bind(&SocketAddr::from((Ipv4Addr::LOCALHOST, 0)).into()).unwrap();
        socket.listen(0).unwrap();
        let address = socket.local_addr().unwrap().as_socket().unwrap();
        let queued = std::net::TcpStream::connect(address).unwrap();
        (socket, queued)
    }

    #[test]
    fn the_first_address_picks_the_family_tried_first() {
        let [a, b, c, d]: [SocketAddr; 4] = ["192.0.2.1:1", "192.0.2.2:1", "[2001:db8::1]:1", "[2001:db8::2]:1"]
            .map(|address| address.parse().unwrap());
        assert_eq!(families(vec![a, c, b, d]), (vec![a, b], vec![c, d]));
        assert_eq!(families(vec![c, a, d]), (vec![c, d], vec![a]));
        assert_eq!(families(vec![a]), (vec![a], vec![]));
    }

    #[test]
    fn each_address_gets_an_equal_share_of_at_least_two_seconds_while_time_lasts() {
        let seconds = Duration::from_secs_f64;
        for (left, addresses, expected) in [(9.0, 2, 4.5), (9.0, 1, 9.0), (9.0, 10, 2.0), (3.0, 2, 2.0), (1.0, 3, 1.0)]
        {
            assert_eq!(share(seconds(left), addresses), seconds(expected), "{left} s for {addresses}");
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_silent_primary_family_lets_the_fallback_in_after_the_delay() {
        let (_silent, queued) = silent();
        let (_open, live) = listener().await;
        let started = Instant::now();
        let primaries = [queued.peer_addr().unwrap()];
        let stream = parallel(&primaries, &[live], Instant::now() + TIMEOUT).await.unwrap();
        assert_eq!(stream.peer_addr().unwrap(), live);
        let elapsed = started.elapsed();
        assert!((FALLBACK_DELAY..FALLBACK_DELAY * 4).contains(&elapsed), "{elapsed:?}");
    }

    #[tokio::test]
    async fn dialled_connections_probe_idle_peers_as_go_does() {
        let (_open, live) = listener().await;
        let stream = dial(&Host::Name("localhost".into()), live.port()).await.unwrap();
        let socket = socket2::SockRef::from(&stream);
        assert!(socket.keepalive().unwrap() && stream.nodelay().unwrap());
        #[cfg(target_os = "linux")]
        assert_eq!(
            (
                socket.tcp_keepalive_time().unwrap(),
                socket.tcp_keepalive_interval().unwrap(),
                socket.tcp_keepalive_retries().unwrap()
            ),
            (KEEP_ALIVE_IDLE, KEEP_ALIVE_INTERVAL, KEEP_ALIVE_PROBES)
        );
    }
}

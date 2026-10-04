//! TCP dialing as Go's dialer does it: staggered attempts across address families, a timeout and keep-alive.
use graphite_meter_proto::origin::Host;
use std::{io, net::SocketAddr, time::Duration};
use tokio::{net::TcpStream, task::JoinSet};

/// A second under Go's 10 s, so the client's 10 s control deadline never ends a dial.
const TIMEOUT: Duration = Duration::from_secs(9);
/// The wait before the next address joins the race.
const STAGGER: Duration = Duration::from_millis(250);
/// Go's keep-alive idle time and probe interval.
const KEEP_ALIVE: Duration = Duration::from_secs(30);

/// The addresses `host` names, without a lookup for an IP address.
pub async fn resolve(host: &Host, port: u16) -> io::Result<Vec<SocketAddr>> {
    match host {
        Host::Ip(ip) => Ok(vec![SocketAddr::new(*ip, port)]),
        Host::Name(name) => Ok(tokio::net::lookup_host((name.as_str(), port)).await?.collect()),
    }
}

/// A connection to the first address of `host` that accepts within the timeout.
pub(crate) async fn dial(host: &Host, port: u16) -> io::Result<TcpStream> {
    let dial = async { race(interleave(resolve(host, port).await?)).await };
    tokio::time::timeout(TIMEOUT, dial)
        .await
        .unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()))
}

/// IPv6 and IPv4 addresses in turn, IPv6 first.
fn interleave(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let (v6, v4): (Vec<_>, Vec<_>) = addresses.into_iter().partition(SocketAddr::is_ipv6);
    (0..v6.len().max(v4.len()))
        .flat_map(|index| v6.get(index).into_iter().chain(v4.get(index)).copied())
        .collect()
}

/// Starts an attempt per address, the next one after the stagger or as soon as one fails; the first to connect wins.
async fn race(addresses: Vec<SocketAddr>) -> io::Result<TcpStream> {
    let mut attempts = JoinSet::new();
    let mut pending = addresses.into_iter();
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address resolved");
    loop {
        if let Some(address) = pending.next() {
            attempts.spawn(TcpStream::connect(address));
        } else if attempts.is_empty() {
            return Err(last);
        }
        tokio::select! {
            Some(attempt) = attempts.join_next() => match attempt.map_err(io::Error::other)? {
                Ok(stream) => {
                    stream.set_nodelay(true)?;
                    keep_alive(&stream);
                    return Ok(stream);
                }
                Err(error) => last = error,
            },
            () = tokio::time::sleep(STAGGER), if pending.len() > 0 => {}
        }
    }
}

/// Probes an idle peer after 30 s and every 30 s after; a socket that refuses the options still connects.
fn keep_alive(stream: &TcpStream) {
    let probes = socket2::TcpKeepalive::new().with_time(KEEP_ALIVE);
    #[cfg(any(target_os = "linux", target_vendor = "apple", windows))]
    let probes = probes.with_interval(KEEP_ALIVE);
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
    fn ipv6_and_ipv4_addresses_alternate_from_ipv6() {
        let [a, b, c, d]: [SocketAddr; 4] = ["192.0.2.1:1", "192.0.2.2:1", "[2001:db8::1]:1", "[2001:db8::2]:1"]
            .map(|address| address.parse().unwrap());
        assert_eq!(interleave(vec![a, b, c]), [c, a, b]);
        assert_eq!(interleave(vec![a, c, d]), [c, a, d]);
    }

    #[tokio::test]
    async fn a_refused_address_hands_over_at_once() {
        let (closed, refused) = listener().await;
        drop(closed);
        let (_open, live) = listener().await;
        let started = Instant::now();
        let stream = race(vec![refused, live]).await.unwrap();
        assert_eq!(stream.peer_addr().unwrap(), live);
        assert!(started.elapsed() < STAGGER, "{:?}", started.elapsed());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_silent_address_hands_over_after_the_stagger() {
        let (_silent, queued) = silent();
        let (_open, live) = listener().await;
        let started = Instant::now();
        let stream = race(vec![queued.peer_addr().unwrap(), live]).await.unwrap();
        assert_eq!(stream.peer_addr().unwrap(), live);
        let elapsed = started.elapsed();
        assert!((STAGGER..STAGGER * 4).contains(&elapsed), "{elapsed:?}");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(start_paused = true)]
    async fn a_dial_nobody_answers_times_out_after_nine_seconds() {
        let (_silent, queued) = silent();
        let address = queued.peer_addr().unwrap();
        let started = Instant::now();
        let refused = dial(&Host::Ip(address.ip()), address.port()).await.unwrap_err();
        assert_eq!((refused.kind(), started.elapsed()), (io::ErrorKind::TimedOut, TIMEOUT));
    }

    #[tokio::test]
    async fn dialled_connections_probe_idle_peers_every_thirty_seconds() {
        let (_open, live) = listener().await;
        let stream = dial(&Host::Name("localhost".into()), live.port()).await.unwrap();
        let socket = socket2::SockRef::from(&stream);
        assert!(socket.keepalive().unwrap() && stream.nodelay().unwrap());
        #[cfg(target_os = "linux")]
        assert_eq!(
            (socket.tcp_keepalive_time().unwrap(), socket.tcp_keepalive_interval().unwrap()),
            (KEEP_ALIVE, KEEP_ALIVE)
        );
    }
}

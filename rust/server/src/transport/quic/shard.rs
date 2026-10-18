//! The Linux endpoint set: `SO_REUSEPORT` sockets on one address. Where the kernel routes by connection ID, a new
//! connection starts on the least loaded endpoint so a latency session and its transfers get separate threads.

use super::HANDSHAKE_BOUND;
use noq::{AsyncUdpSocket, ConnectionId, ConnectionIdGenerator, InvalidCid, UdpSender, udp::RecvMeta};
use std::{
    collections::VecDeque,
    hash::{BuildHasher, RandomState},
    io::{self, IoSliceMut},
    net::SocketAddr,
    num::NonZeroUsize,
    ops::RangeInclusive,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

/// Forwarded datagrams an endpoint queues; a full queue drops more, as a full socket buffer would.
const QUEUE_DATAGRAMS: usize = 1024;
/// New connections a socket keeps the endpoint of; more forget the oldest.
const FIRST_FLIGHTS: usize = 64;
/// Set in the first byte of long-header packets: Initial, 0-RTT, Handshake, Retry and version negotiation.
const LONG_HEADER: u8 = 0x80;
/// noq's hashed connection ID: a random nonce, then a keyed hash of it.
const NONCE_BYTES: usize = 3;
const HASHED_CID_BYTES: usize = 8;
/// The set's connection IDs: the endpoint's index, then the hashed ID.
const CID_BYTES: usize = 1 + HASHED_CID_BYTES;
/// The IDs a client may choose for a new connection's first packets (RFC 9000, 7.2).
const CLIENT_CID_BYTES: RangeInclusive<usize> = 8..=20;

/// A factory for `EndpointConfig::cid_generator` whose connection IDs name `shard`.
pub(super) fn cid_generator(shard: u8) -> Arc<dyn Fn() -> Box<dyn ConnectionIdGenerator> + Send + Sync> {
    Arc::new(move || Box::new(ShardCids { shard, hash: RandomState::new() }))
}

/// The shard's index, then noq's hashed connection ID format, which noq does not export.
struct ShardCids {
    shard: u8,
    hash: RandomState,
}

impl ShardCids {
    fn signature(&self, nonce: &[u8]) -> [u8; 8] {
        self.hash.hash_one(nonce).to_le_bytes()
    }
}

impl ConnectionIdGenerator for ShardCids {
    fn generate_cid(&mut self) -> ConnectionId {
        let mut cid = [0; CID_BYTES];
        cid[0] = self.shard;
        let (nonce, signature) = cid[1..].split_at_mut(NONCE_BYTES);
        nonce.copy_from_slice(&crate::random::<NONCE_BYTES>());
        signature.copy_from_slice(&self.signature(nonce)[..signature.len()]);
        ConnectionId::new(&cid)
    }

    fn validate(&self, cid: ConnectionId) -> Result<(), InvalidCid> {
        if cid.len() != self.cid_len() || cid[0] != self.shard {
            return Err(InvalidCid);
        }
        let (nonce, signature) = cid[1..].split_at(NONCE_BYTES);
        (self.signature(nonce)[..signature.len()] == *signature)
            .then_some(())
            .ok_or(InvalidCid)
    }

    fn cid_len(&self) -> usize {
        CID_BYTES
    }

    fn cid_lifetime(&self) -> Option<Duration> {
        None
    }
}

/// A datagram another endpoint received for this one.
#[derive(Debug)]
struct Forwarded {
    meta: RecvMeta,
    datagram: Box<[u8]>,
}

/// An endpoint's forwarding at its worst: every queue slot holding a datagram at the endpoints' payload limit, and
/// every new connection its socket keeps.
pub(super) fn forwarding_bytes(max_datagram: usize) -> Option<usize> {
    max_datagram
        .checked_add(size_of::<Forwarded>())?
        .checked_mul(QUEUE_DATAGRAMS)?
        .checked_add(size_of::<FirstFlight>().checked_mul(FIRST_FLIGHTS)?)
}

/// Has the kernel deliver each datagram to the group socket its connection ID names (see `named`), else hash it.
/// Sockets join in endpoint order, so a socket's index is its endpoint's.
pub(super) fn route_in_kernel(socket: &std::net::UdpSocket) -> io::Result<()> {
    cfg_select! {
        target_os = "linux" => {
            use nix::{
                libc::{BPF_A, BPF_ABS, BPF_B, BPF_JEQ, BPF_JMP, BPF_JSET, BPF_K, BPF_LD, BPF_RET, sock_filter, sock_fprog},
                sys::socket::{setsockopt, sockopt::AttachReusePortCbpf},
            };
            const fn op(code: u32, jt: u8, jf: u8, k: u32) -> sock_filter {
                sock_filter { code: code as u16, jt, jf, k }
            }
            const fn load(offset: u32) -> sock_filter {
                op(BPF_LD | BPF_B | BPF_ABS, 0, 0, offset)
            }
            // Offsets count from the UDP payload; a jump skips its `jt` or `jf` instructions.
            let mut filter = [
                load(0),
                op(BPF_JMP | BPF_JSET | BPF_K, 2, 0, LONG_HEADER as u32),
                load(1),
                op(BPF_RET | BPF_A, 0, 0, 0),
                load(5),
                op(BPF_JMP | BPF_JEQ | BPF_K, 0, 2, CID_BYTES as u32),
                load(6),
                op(BPF_RET | BPF_A, 0, 0, 0),
                op(BPF_RET | BPF_K, 0, 0, u32::MAX),
            ];
            let program = sock_fprog { len: filter.len() as u16, filter: filter.as_mut_ptr() };
            setsockopt(socket, AttachReusePortCbpf, &program).map_err(io::Error::from)
        }
        _ => {
            let _ = socket;
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}

/// Every endpoint's queue, which each socket forwards into, and connections, which steer new ones when the kernel
/// routes.
#[derive(Debug, Clone)]
pub(super) struct Router {
    queues: Arc<[mpsc::Sender<Forwarded>]>,
    connections: Arc<[AtomicUsize]>,
    steering: bool,
    max_datagram: usize,
}

/// An endpoint's count of connections, which each of them holds a share of while it lasts.
#[derive(Debug, Clone)]
pub(super) struct Shard {
    connections: Arc<[AtomicUsize]>,
    index: usize,
}

impl Shard {
    pub(super) fn occupy(&self) -> Occupant {
        self.connections[self.index].fetch_add(1, Ordering::Relaxed);
        Occupant(self.clone())
    }
}

/// A connection's share of its endpoint's count.
#[derive(Debug)]
pub(super) struct Occupant(Shard);

impl Drop for Occupant {
    fn drop(&mut self) {
        self.0.connections[self.0.index].fetch_sub(1, Ordering::Relaxed);
    }
}

/// What a datagram's destination connection ID says of its endpoint.
#[derive(Debug, PartialEq)]
enum Named {
    /// One of the set's IDs, naming its endpoint, or an endpoint past the set.
    Endpoint(usize),
    /// A client's own ID: a new connection's first packets.
    Client(ConnectionId),
    Nothing,
}

/// A short header's ID names its endpoint in byte 1; a long header's ID follows its length in byte 5.
fn named(datagram: &[u8]) -> Named {
    match *datagram {
        [first, named, ..] if first & LONG_HEADER == 0 => Named::Endpoint(usize::from(named)),
        [_, _, _, _, _, len, ref rest @ ..] => match rest.get(..usize::from(len)) {
            Some(id) if id.len() == CID_BYTES => Named::Endpoint(usize::from(id[0])),
            Some(id) if CLIENT_CID_BYTES.contains(&id.len()) => Named::Client(ConnectionId::new(id)),
            _ => Named::Nothing,
        },
        _ => Named::Nothing,
    }
}

/// A new connection's client ID, the endpoint its first packet went to, and when.
type FirstFlight = (ConnectionId, usize, Instant);

/// The endpoints a socket sent new connections to, so that every packet of a first flight, resent ones too, follows
/// the first while its handshake may last.
#[derive(Debug)]
struct FirstFlights(VecDeque<FirstFlight>);

impl FirstFlights {
    fn new() -> Self {
        Self(VecDeque::with_capacity(FIRST_FLIGHTS))
    }

    fn endpoint(&mut self, id: ConnectionId, steer: impl FnOnce() -> usize) -> usize {
        let now = Instant::now();
        let ended = self
            .0
            .partition_point(|&(.., at)| now.duration_since(at) > HANDSHAKE_BOUND);
        self.0.drain(..ended);
        if let Some(&(_, to, _)) = self.0.iter().find(|(known, ..)| *known == id) {
            return to;
        }
        if self.0.len() == FIRST_FLIGHTS {
            self.0.pop_front();
        }
        let to = steer();
        self.0.push_back((id, to, now));
        to
    }
}

/// The datagrams other endpoints forwarded to one.
#[derive(Debug)]
pub(super) struct Inbox(mpsc::Receiver<Forwarded>);

impl Router {
    /// Queues for `shards` endpoints forwarding datagrams within their payload limit, steering if the kernel routes.
    pub(super) fn new(shards: usize, max_datagram: usize, steering: bool) -> (Self, Vec<Inbox>) {
        let (queues, inboxes): (Vec<_>, _) = (0..shards)
            .map(|_| {
                let (queue, inbox) = mpsc::channel(QUEUE_DATAGRAMS);
                (queue, Inbox(inbox))
            })
            .unzip();
        let connections = (0..shards).map(|_| AtomicUsize::new(0)).collect();
        (Self { queues: queues.into(), connections, steering, max_datagram }, inboxes)
    }

    pub(super) fn shard(&self, index: usize) -> Shard {
        Shard { connections: self.connections.clone(), index }
    }

    /// Where a new connection at `shard` runs: the endpoint holding the fewest connections, `shard` among equals.
    fn steer(&self, shard: usize) -> usize {
        let held = |index: usize| self.connections[index].load(Ordering::Relaxed);
        (0..self.connections.len()).fold(shard, |least, index| if held(index) < held(least) { index } else { least })
    }

    /// The other endpoint a datagram goes to: the one its ID names, or the one its new connection went to.
    fn destination(&self, shard: usize, flights: &mut FirstFlights, datagram: &[u8]) -> Option<usize> {
        let to = match named(datagram) {
            Named::Endpoint(to) => to,
            Named::Client(id) if self.steering => flights.endpoint(id, || self.steer(shard)),
            Named::Client(_) | Named::Nothing => shard,
        };
        (to != shard && to < self.queues.len()).then_some(to)
    }

    /// Forwards received segments for another endpoint, moving the rest together; only the last may be short (GRO).
    fn route(&self, shard: usize, flights: &mut FirstFlights, buf: &mut [u8], meta: &mut RecvMeta) {
        let (len, stride) = (meta.len.min(buf.len()), meta.stride.max(1));
        let mut kept = 0;
        for start in (0..len).step_by(stride) {
            let segment = start..len.min(start.saturating_add(stride));
            if let Some(to) = self.destination(shard, flights, &buf[segment.clone()]) {
                self.forward(to, &buf[segment], meta);
                continue;
            }
            if start != kept {
                buf.copy_within(segment.clone(), kept);
            }
            kept += segment.len();
        }
        meta.len = kept;
    }

    /// Queues one datagram for its endpoint, or drops it as UDP would.
    fn forward(&self, shard: usize, datagram: &[u8], meta: &RecvMeta) {
        if datagram.len() > self.max_datagram {
            return;
        }
        let mut meta = *meta;
        (meta.len, meta.stride) = (datagram.len(), datagram.len());
        let _ = self.queues[shard].try_send(Forwarded { meta, datagram: datagram.into() });
    }
}

/// An endpoint's socket: delivers what other endpoints forwarded to it, then receives, forwarding what names another.
#[derive(Debug)]
pub(super) struct ShardSocket {
    socket: Box<dyn AsyncUdpSocket>,
    shard: usize,
    router: Router,
    inbox: Inbox,
    flights: FirstFlights,
}

impl ShardSocket {
    pub(super) fn new(socket: Box<dyn AsyncUdpSocket>, shard: usize, router: Router, inbox: Inbox) -> Self {
        Self { socket, shard, router, inbox, flights: FirstFlights::new() }
    }
}

impl AsyncUdpSocket for ShardSocket {
    fn create_sender(&self) -> Pin<Box<dyn UdpSender>> {
        self.socket.create_sender()
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let slots = bufs.len().min(meta.len());
        // Forwarded datagrams fill at most half the slots so that the socket keeps its turn; an empty queue registers
        // the waker forwarding wakes.
        let mut filled = 0;
        while filled < slots.div_ceil(2) {
            let Poll::Ready(Some(forwarded)) = self.inbox.0.poll_recv(cx) else {
                break;
            };
            if let Some(buf) = bufs[filled].get_mut(..forwarded.datagram.len()) {
                buf.copy_from_slice(&forwarded.datagram);
                meta[filled] = forwarded.meta;
                filled += 1;
            }
        }
        if filled < slots {
            let received = self
                .socket
                .poll_recv(cx, &mut bufs[filled..slots], &mut meta[filled..slots]);
            match received {
                Poll::Ready(Ok(received)) => {
                    for (buf, info) in bufs[filled..].iter_mut().zip(&mut meta[filled..]).take(received) {
                        self.router.route(self.shard, &mut self.flights, buf, info);
                    }
                    filled += received;
                }
                // Forwarded datagrams go first; the next call reads the socket again.
                Poll::Ready(Err(error)) if filled == 0 => return Poll::Ready(Err(error)),
                _ => {}
            }
        }
        if filled == 0 { Poll::Pending } else { Poll::Ready(Ok(filled)) }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    fn max_receive_segments(&self) -> NonZeroUsize {
        self.socket.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.socket.may_fragment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4433);

    /// A short-header packet whose destination connection ID begins with `shard`.
    fn short(shard: u8, len: usize) -> Vec<u8> {
        let mut packet = vec![0x40, shard];
        packet.resize(len, 0xa5);
        packet
    }

    fn received(len: usize, stride: usize) -> RecvMeta {
        let mut meta = RecvMeta::default();
        (meta.addr, meta.len, meta.stride) = (PEER, len, stride);
        meta
    }

    /// A long-header packet whose destination connection ID is `id`.
    fn long(id: &[u8], len: usize) -> Vec<u8> {
        let mut packet = vec![0xc0, 0, 0, 0, 1, u8::try_from(id.len()).unwrap()];
        packet.extend_from_slice(id);
        packet.resize(len, 0xa5);
        packet
    }

    fn drain(inbox: &mut Inbox) -> Vec<Forwarded> {
        std::iter::from_fn(|| inbox.0.try_recv().ok()).collect()
    }

    /// Routes `packet` as received by `shard`; whether it stayed.
    fn stays(router: &Router, shard: usize, flights: &mut FirstFlights, packet: &[u8]) -> bool {
        let mut buf = packet.to_vec();
        let mut meta = received(buf.len(), buf.len());
        router.route(shard, flights, &mut buf, &mut meta);
        meta.len == packet.len()
    }

    #[test]
    fn a_long_header_carrying_an_id_of_the_set_goes_to_the_endpoint_it_names() {
        let (router, mut inboxes) = Router::new(4, 1472, true);
        let mut flights = FirstFlights::new();
        let ours = cid_generator(2)().generate_cid();
        assert!(!stays(&router, 1, &mut flights, &long(&ours, 60)));
        assert!(stays(&router, 2, &mut flights, &long(&ours, 60)));
        // A length byte past the datagram, or an ID shorter than a client may choose, names nothing.
        assert!(stays(&router, 1, &mut flights, &long(&ours, 60)[..10]));
        assert!(stays(&router, 1, &mut flights, &long(&[2; 4], 60)));
        assert_eq!(drain(&mut inboxes[2]).len(), 1);
        assert!(flights.0.is_empty());
    }

    #[test]
    fn a_new_connection_starts_on_the_least_held_endpoint_with_all_its_first_packets() {
        let (router, mut inboxes) = Router::new(3, 1472, true);
        let mut flights = FirstFlights::new();
        let first = long(&[7; 8], 1200);
        // Every endpoint is empty: the connection stays.
        assert!(stays(&router, 1, &mut flights, &first));
        let held = router.shard(1).occupy();
        // It keeps its endpoint once that holds it, and so does every packet of its first flight.
        assert!(stays(&router, 1, &mut flights, &first));
        // A second connection at endpoint 1 goes to endpoint 0, the first of the least held, and its resent packets
        // follow, though endpoint 0 holds it by then and endpoint 2 is empty.
        let second = long(&[9; 20], 1200);
        assert!(!stays(&router, 1, &mut flights, &second));
        let _second = router.shard(0).occupy();
        assert!(!stays(&router, 1, &mut flights, &second));
        assert_eq!(drain(&mut inboxes[0]).len(), 2);
        // With every endpoint holding as many, a new one stays where it arrived.
        let _third = router.shard(2).occupy();
        assert!(stays(&router, 1, &mut flights, &long(&[11; 8], 1200)));
        drop(held);
        assert_eq!(router.connections[1].load(Ordering::Relaxed), 0);
        assert!(inboxes.iter_mut().all(|inbox| drain(inbox).is_empty()));
    }

    #[test]
    fn without_kernel_routing_a_new_connection_stays_where_it_arrived() {
        let (router, mut inboxes) = Router::new(2, 1472, false);
        let mut flights = FirstFlights::new();
        let _held = router.shard(1).occupy();
        assert!(stays(&router, 1, &mut flights, &long(&[7; 8], 1200)));
        assert!(flights.0.is_empty());
        assert!(inboxes.iter_mut().all(|inbox| drain(inbox).is_empty()));
    }

    #[test]
    fn a_socket_keeps_the_newest_first_flights() {
        let (router, _inboxes) = Router::new(2, 1472, true);
        let mut flights = FirstFlights::new();
        let _held = router.shard(1).occupy();
        for client in 0..=FIRST_FLIGHTS {
            let id = [u8::try_from(client).unwrap(); 8];
            assert!(!stays(&router, 1, &mut flights, &long(&id, 1200)));
        }
        assert_eq!(flights.0.len(), FIRST_FLIGHTS);
        assert!(flights.0.iter().all(|(id, ..)| id[0] != 0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_kernel_delivers_a_datagram_to_the_socket_its_id_names() {
        let bind = |address| graphite_meter_net::bind_udp(address, 2).unwrap().0;
        let first = bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0));
        let address = first.local_addr().unwrap();
        let sockets = [first, bind(address)];
        route_in_kernel(&sockets[0]).unwrap();
        for socket in &sockets {
            socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        }
        let id = |shard| cid_generator(shard)().generate_cid();
        for (packet, to) in [(short(1, 40), 1), (short(0, 40), 0), (long(&id(1), 60), 1), (long(&id(0), 60), 0)] {
            // Each client port hashes to either socket; the ID decides.
            for _ in 0..8 {
                let client = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
                client.send_to(&packet, address).unwrap();
                let mut buf = [0; 64];
                assert_eq!(sockets[to].recv(&mut buf).unwrap(), packet.len(), "{:02x?}", &packet[..2]);
            }
        }
    }

    #[test]
    fn a_short_header_leaves_for_the_endpoint_it_names() {
        let (router, mut inboxes) = Router::new(4, 1472, true);
        let mut flights = FirstFlights::new();
        let mut long = short(2, 40);
        long[0] = 0xc0;
        for (packet, stays) in [
            (short(2, 40), false),
            (short(1, 40), true),
            (short(200, 40), true),
            (long, true),
            (vec![0x40], true),
            // Past the endpoints' payload limit, which no queue slot holds: dropped.
            (short(3, 1473), false),
        ] {
            let mut buf = packet.clone();
            let mut meta = received(buf.len(), buf.len());
            router.route(1, &mut flights, &mut buf, &mut meta);
            let (kept, head) = (if stays { packet.len() } else { 0 }, &packet[..2.min(packet.len())]);
            assert_eq!(meta.len, kept, "{head:02x?}");
            assert_eq!(buf, packet);
        }
        let forwarded = drain(&mut inboxes[2]);
        assert_eq!(forwarded.len(), 1);
        assert_eq!(*forwarded[0].datagram, short(2, 40));
        let meta = forwarded[0].meta;
        assert_eq!((meta.addr, meta.len, meta.stride), (PEER, 40, 40));
        assert!(inboxes.iter_mut().all(|inbox| drain(inbox).is_empty()));
    }
}

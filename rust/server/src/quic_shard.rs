//! QUIC shards: one endpoint per runtime thread, each on its own socket bound to one address with `SO_REUSEPORT`.
//!
//! The kernel picks a shard by hashing a datagram's 4-tuple, so after NAT rebinding or migration a connection's
//! packets can reach another shard. Connection IDs therefore begin with their shard's index, and each shard's socket
//! forwards the short-header packets that name another shard to that shard's queue.

use noq::{AsyncUdpSocket, ConnectionId, ConnectionIdGenerator, InvalidCid, UdpSender, udp::RecvMeta};
use std::{
    hash::{BuildHasher, RandomState},
    io::{self, IoSliceMut},
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::mpsc;

/// A shard index is one connection ID byte.
pub(crate) const MAX_SHARDS: usize = 1 << u8::BITS;
/// Forwarded datagrams a shard queues; a full queue drops more, as a full socket buffer would.
const QUEUE_DATAGRAMS: usize = 1024;
/// Set in the first byte of long-header packets: Initial, 0-RTT, Handshake, Retry and version negotiation.
const LONG_HEADER: u8 = 0x80;
/// noq's hashed connection ID: a random nonce, then a keyed hash of it.
const NONCE_BYTES: usize = 3;
const HASHED_CID_BYTES: usize = 8;

/// A factory for `EndpointConfig::cid_generator` whose connection IDs name `shard`.
pub(crate) fn cid_generator(shard: u8) -> Arc<dyn Fn() -> Box<dyn ConnectionIdGenerator> + Send + Sync> {
    Arc::new(move || {
        Box::new(ShardCids {
            shard,
            inner: Box::new(HashedCids(RandomState::new())),
        })
    })
}

/// The inner generator's connection IDs behind the shard index, which only this shard validates.
struct ShardCids {
    shard: u8,
    inner: Box<dyn ConnectionIdGenerator>,
}

impl ConnectionIdGenerator for ShardCids {
    fn generate_cid(&mut self) -> ConnectionId {
        let inner = self.inner.generate_cid();
        ConnectionId::new(&[&[self.shard][..], &inner[..]].concat())
    }

    fn validate(&self, cid: ConnectionId) -> Result<(), InvalidCid> {
        match cid.split_first() {
            Some((&shard, rest)) if shard == self.shard && rest.len() == self.inner.cid_len() => {
                self.inner.validate(ConnectionId::new(rest))
            }
            _ => Err(InvalidCid),
        }
    }

    fn cid_len(&self) -> usize {
        1 + self.inner.cid_len()
    }

    fn cid_lifetime(&self) -> Option<Duration> {
        self.inner.cid_lifetime()
    }
}

/// noq's default `HashedConnectionIdGenerator`, which noq does not export, with a SipHash key.
struct HashedCids(RandomState);

impl HashedCids {
    fn signature(&self, nonce: &[u8]) -> [u8; 8] {
        self.0.hash_one(nonce).to_le_bytes()
    }
}

impl ConnectionIdGenerator for HashedCids {
    fn generate_cid(&mut self) -> ConnectionId {
        let mut cid = [0; HASHED_CID_BYTES];
        let (nonce, signature) = cid.split_at_mut(NONCE_BYTES);
        getrandom::fill(nonce).expect("randomness for QUIC connection IDs");
        signature.copy_from_slice(&self.signature(nonce)[..signature.len()]);
        ConnectionId::new(&cid)
    }

    fn validate(&self, cid: ConnectionId) -> Result<(), InvalidCid> {
        let (nonce, signature) = cid.split_at_checked(NONCE_BYTES).ok_or(InvalidCid)?;
        (cid.len() == HASHED_CID_BYTES && self.signature(nonce)[..signature.len()] == *signature)
            .then_some(())
            .ok_or(InvalidCid)
    }

    fn cid_len(&self) -> usize {
        HASHED_CID_BYTES
    }

    fn cid_lifetime(&self) -> Option<Duration> {
        None
    }
}

/// A datagram another shard received for this one.
#[derive(Debug)]
struct Forwarded {
    meta: RecvMeta,
    datagram: Box<[u8]>,
}

/// A shard's worst-case queue: every slot holding a datagram at the endpoints' payload limit.
pub(crate) fn queue_bytes(max_datagram: usize) -> Option<usize> {
    max_datagram
        .checked_add(size_of::<Forwarded>())?
        .checked_mul(QUEUE_DATAGRAMS)
}

/// Every shard's queue, which each shard's socket forwards into.
#[derive(Debug, Clone)]
pub(crate) struct Router {
    queues: Arc<[mpsc::Sender<Forwarded>]>,
    max_datagram: usize,
    forwarded: Arc<AtomicUsize>,
}

/// The datagrams other shards forwarded to one shard.
#[derive(Debug)]
pub(crate) struct Inbox(mpsc::Receiver<Forwarded>);

impl Router {
    /// Queues for `shards` shards, forwarding datagrams up to the endpoints' payload limit.
    pub(crate) fn new(shards: usize, max_datagram: usize) -> (Self, Vec<Inbox>) {
        let (queues, inboxes): (Vec<_>, _) = (0..shards)
            .map(|_| {
                let (queue, inbox) = mpsc::channel(QUEUE_DATAGRAMS);
                (queue, Inbox(inbox))
            })
            .unzip();
        let router = Self {
            queues: queues.into(),
            max_datagram,
            forwarded: Arc::default(),
        };
        (router, inboxes)
    }

    /// Datagrams queued for another shard so far.
    #[cfg(test)]
    pub(crate) fn forwarded(&self) -> usize {
        self.forwarded.load(Ordering::Relaxed)
    }

    /// The other shard a short-header packet's destination connection ID names. Long headers stay: the handshake
    /// cannot migrate, and their connection IDs may be the client's.
    fn destination(&self, shard: usize, datagram: &[u8]) -> Option<usize> {
        match *datagram {
            [first, named, ..] if first & LONG_HEADER == 0 => {
                let named = usize::from(named);
                (named != shard && named < self.queues.len()).then_some(named)
            }
            _ => None,
        }
    }

    /// Forwards each segment of a received buffer that names another shard and moves the rest together, so a
    /// buffer whose segments all left is empty. Only the last segment may be shorter than the stride, as in GRO.
    fn route(&self, shard: usize, buf: &mut [u8], meta: &mut RecvMeta) {
        let (len, stride) = (meta.len.min(buf.len()), meta.stride.max(1));
        let mut kept = 0;
        for start in (0..len).step_by(stride) {
            let segment = start..len.min(start.saturating_add(stride));
            if let Some(to) = self.destination(shard, &buf[segment.clone()]) {
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

    /// Queues one datagram for its shard, or drops it as UDP would.
    fn forward(&self, shard: usize, datagram: &[u8], meta: &RecvMeta) {
        if datagram.len() > self.max_datagram {
            return;
        }
        let mut meta = *meta;
        (meta.len, meta.stride) = (datagram.len(), datagram.len());
        let forwarded = Forwarded {
            meta,
            datagram: datagram.into(),
        };
        if self.queues[shard].try_send(forwarded).is_ok() {
            self.forwarded.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A shard's socket: delivers what other shards forwarded to it, then receives, forwarding what names another shard.
#[derive(Debug)]
pub(crate) struct ShardSocket {
    socket: Box<dyn AsyncUdpSocket>,
    shard: usize,
    router: Router,
    inbox: Inbox,
}

impl ShardSocket {
    pub(crate) fn new(socket: Box<dyn AsyncUdpSocket>, shard: usize, router: Router, inbox: Inbox) -> Self {
        Self {
            socket,
            shard,
            router,
            inbox,
        }
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
        // Forwarded datagrams fill at most half the slots, so the shard's own socket keeps its turn. An empty
        // queue registers the waker that forwarding wakes.
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
            match self
                .socket
                .poll_recv(cx, &mut bufs[filled..slots], &mut meta[filled..slots])
            {
                Poll::Ready(Ok(received)) => {
                    for (buf, info) in bufs[filled..].iter_mut().zip(&mut meta[filled..]).take(received) {
                        self.router.route(self.shard, buf, info);
                    }
                    filled += received;
                }
                // Forwarded datagrams go first; the next call reads the socket again.
                Poll::Ready(Err(error)) if filled == 0 => return Poll::Ready(Err(error)),
                _ => {}
            }
        }
        if filled == 0 {
            Poll::Pending
        } else {
            Poll::Ready(Ok(filled))
        }
    }

    fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
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
    use std::{net::SocketAddr, sync::atomic::AtomicBool, task::Wake};

    const PEER: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 4433);

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

    fn drain(inbox: &mut Inbox) -> Vec<Forwarded> {
        std::iter::from_fn(|| inbox.0.try_recv().ok()).collect()
    }

    #[test]
    fn connection_ids_name_their_shard_and_validate_only_there() {
        let mut first = cid_generator(1).as_ref()();
        let second = cid_generator(2).as_ref()();
        let cid = first.generate_cid();
        assert_eq!(first.cid_len(), 1 + HASHED_CID_BYTES);
        assert_eq!(cid.len(), first.cid_len());
        assert_eq!(cid[0], 1);
        assert_ne!(first.generate_cid(), cid, "a fresh nonce for every connection ID");
        first.validate(cid).unwrap();
        assert!(second.validate(cid).is_err(), "another shard's connection ID");
        let mut renamed = cid;
        renamed[0] = 2;
        assert!(
            second.validate(renamed).is_err(),
            "a shard byte alone does not validate"
        );
        let mut forged = cid;
        forged[HASHED_CID_BYTES] ^= 1;
        assert!(
            first.validate(forged).is_err(),
            "a signature that does not match its nonce"
        );
        assert!(first.validate(ConnectionId::new(&cid[..HASHED_CID_BYTES])).is_err());
        assert_eq!(first.cid_lifetime(), None);
    }

    #[test]
    fn only_short_headers_naming_another_shard_leave() {
        let (router, mut inboxes) = Router::new(4, 1472);
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
            router.route(1, &mut buf, &mut meta);
            assert_eq!(
                meta.len,
                if stays { packet.len() } else { 0 },
                "{:02x?}",
                &packet[..2.min(packet.len())]
            );
            assert_eq!(buf, packet);
        }
        let forwarded = drain(&mut inboxes[2]);
        assert_eq!(forwarded.len(), 1);
        assert_eq!(*forwarded[0].datagram, short(2, 40));
        let meta = forwarded[0].meta;
        assert_eq!((meta.addr, meta.len, meta.stride), (PEER, 40, 40));
        assert!(inboxes.iter_mut().all(|inbox| drain(inbox).is_empty()));
        assert_eq!(router.forwarded(), 1);
    }

    #[test]
    fn gro_batches_route_each_segment_and_keep_the_rest_in_order() {
        let (router, mut inboxes) = Router::new(4, 1472);
        let mut long = short(3, 10);
        long[0] = 0xc0;
        let segments = [short(0, 10), short(2, 10), long.clone(), short(0, 10), short(3, 6)];
        let mut buf = segments.concat();
        buf.resize(64, 0);
        let mut meta = received(46, 10);
        router.route(0, &mut buf, &mut meta);
        assert_eq!(meta.len, 30);
        assert_eq!(meta.stride, 10);
        assert_eq!(buf[..30], [short(0, 10), long, short(0, 10)].concat());
        let two = drain(&mut inboxes[2]);
        let three = drain(&mut inboxes[3]);
        assert_eq!(
            two.iter().map(|f| &*f.datagram).collect::<Vec<_>>(),
            [&short(2, 10)[..]]
        );
        assert_eq!(
            three.iter().map(|f| &*f.datagram).collect::<Vec<_>>(),
            [&short(3, 6)[..]]
        );
        assert_eq!((three[0].meta.len, three[0].meta.stride), (6, 6));

        // A short last segment moves up behind the kept ones.
        let mut buf = [short(1, 10), short(0, 10), short(0, 4)].concat();
        let mut meta = received(24, 10);
        router.route(0, &mut buf, &mut meta);
        assert_eq!(meta.len, 14);
        assert_eq!(buf[..14], [short(0, 10), short(0, 4)].concat());
        assert_eq!(drain(&mut inboxes[1]).len(), 1);
    }

    #[test]
    fn a_full_queue_drops_further_datagrams() {
        let (router, mut inboxes) = Router::new(2, 1472);
        for _ in 0..=QUEUE_DATAGRAMS {
            let mut buf = short(1, 40);
            let mut meta = received(40, 40);
            router.route(0, &mut buf, &mut meta);
            assert_eq!(meta.len, 0, "a dropped datagram does not stay either");
        }
        assert_eq!(router.forwarded(), QUEUE_DATAGRAMS);
        assert_eq!(drain(&mut inboxes[1]).len(), QUEUE_DATAGRAMS);
    }

    struct Woken(AtomicBool);

    impl Wake for Woken {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn forwarding_wakes_the_shard_whose_socket_delivers_it_before_its_own() {
        let runtime = noq::default_runtime().unwrap();
        let socket = runtime
            .wrap_udp_socket(std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
            .unwrap();
        let address = socket.local_addr().unwrap();
        let (router, mut inboxes) = Router::new(2, 1472);
        let mut shard = ShardSocket::new(socket, 0, router.clone(), inboxes.remove(0));
        let mut storage = vec![0; 4 * 1472];
        let mut bufs: Vec<_> = storage.chunks_mut(1472).map(IoSliceMut::new).collect();
        let mut meta = [RecvMeta::default(); 4];
        let woken = Arc::new(Woken(AtomicBool::new(false)));
        let waker = std::task::Waker::from(woken.clone());
        let mut cx = Context::from_waker(&waker);
        assert!(shard.poll_recv(&mut cx, &mut bufs, &mut meta).is_pending());

        let local = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        local.send_to(&short(0, 30), address).unwrap();
        let mut buf = short(0, 20);
        let mut info = received(20, 20);
        router.route(1, &mut buf, &mut info);
        assert!(woken.0.load(Ordering::Relaxed), "forwarding left the shard asleep");
        let mut arrived = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while arrived.len() < 2 {
                let count = std::future::poll_fn(|cx| shard.poll_recv(cx, &mut bufs, &mut meta))
                    .await
                    .unwrap();
                arrived.extend((0..count).map(|slot| (meta[slot].addr, bufs[slot][..meta[slot].len].to_vec())));
            }
        })
        .await
        .unwrap();
        assert_eq!(
            arrived[0],
            (PEER, short(0, 20)),
            "the forwarded datagram, with its peer, first"
        );
        assert_eq!(arrived[1], (local.local_addr().unwrap(), short(0, 30)));
    }
}

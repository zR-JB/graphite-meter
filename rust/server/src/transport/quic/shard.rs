//! The Linux endpoint set: `SO_REUSEPORT` sockets on one address, forwarding packets whose connection ID names another.

use noq::{AsyncUdpSocket, ConnectionId, ConnectionIdGenerator, InvalidCid, UdpSender, udp::RecvMeta};
use std::{
    hash::{BuildHasher, RandomState},
    io::{self, IoSliceMut},
    net::SocketAddr,
    num::NonZeroUsize,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::mpsc;

/// Forwarded datagrams an endpoint queues; a full queue drops more, as a full socket buffer would.
const QUEUE_DATAGRAMS: usize = 1024;
/// Set in the first byte of long-header packets: Initial, 0-RTT, Handshake, Retry and version negotiation.
const LONG_HEADER: u8 = 0x80;
/// noq's hashed connection ID: a random nonce, then a keyed hash of it.
const NONCE_BYTES: usize = 3;
const HASHED_CID_BYTES: usize = 8;

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
        let mut cid = [0; 1 + HASHED_CID_BYTES];
        cid[0] = self.shard;
        let (nonce, signature) = cid[1..].split_at_mut(NONCE_BYTES);
        getrandom::fill(nonce).expect("randomness for QUIC connection IDs");
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
        1 + HASHED_CID_BYTES
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

/// An endpoint's queue at its worst: every slot holding a datagram at the endpoints' payload limit.
pub(super) fn queue_bytes(max_datagram: usize) -> Option<usize> {
    max_datagram
        .checked_add(size_of::<Forwarded>())?
        .checked_mul(QUEUE_DATAGRAMS)
}

/// Every endpoint's queue, which each socket forwards into.
#[derive(Debug, Clone)]
pub(super) struct Router {
    queues: Arc<[mpsc::Sender<Forwarded>]>,
    max_datagram: usize,
}

/// The datagrams other endpoints forwarded to one.
#[derive(Debug)]
pub(super) struct Inbox(mpsc::Receiver<Forwarded>);

impl Router {
    /// Queues for `shards` endpoints, forwarding datagrams up to the endpoints' payload limit.
    pub(super) fn new(shards: usize, max_datagram: usize) -> (Self, Vec<Inbox>) {
        let (queues, inboxes): (Vec<_>, _) = (0..shards)
            .map(|_| {
                let (queue, inbox) = mpsc::channel(QUEUE_DATAGRAMS);
                (queue, Inbox(inbox))
            })
            .unzip();
        (Self { queues: queues.into(), max_datagram }, inboxes)
    }

    /// The other endpoint a short-header packet's destination connection ID names; handshakes never migrate, and
    /// long headers may carry the client's connection IDs.
    fn destination(&self, shard: usize, datagram: &[u8]) -> Option<usize> {
        match *datagram {
            [first, named, ..] if first & LONG_HEADER == 0 => {
                let named = usize::from(named);
                (named != shard && named < self.queues.len()).then_some(named)
            }
            _ => None,
        }
    }

    /// Forwards each segment of a received buffer that names another endpoint and moves the rest together; only the
    /// last segment may be shorter than the stride, as in GRO.
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
}

impl ShardSocket {
    pub(super) fn new(socket: Box<dyn AsyncUdpSocket>, shard: usize, router: Router, inbox: Inbox) -> Self {
        Self { socket, shard, router, inbox }
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
                        self.router.route(self.shard, buf, info);
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

    fn drain(inbox: &mut Inbox) -> Vec<Forwarded> {
        std::iter::from_fn(|| inbox.0.try_recv().ok()).collect()
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

    #[test]
    fn gro_batches_route_each_segment_and_keep_the_rest_in_order() {
        let (router, mut inboxes) = Router::new(4, 1472);
        let mut long = short(3, 10);
        long[0] = 0xc0;
        let mut buf = [short(0, 10), short(2, 10), long.clone(), short(0, 10), short(3, 6)].concat();
        buf.resize(64, 0);
        let mut meta = received(46, 10);
        router.route(0, &mut buf, &mut meta);
        assert_eq!((meta.len, meta.stride), (30, 10));
        assert_eq!(buf[..30], [short(0, 10), long, short(0, 10)].concat());
        let (two, three) = (drain(&mut inboxes[2]), drain(&mut inboxes[3]));
        assert_eq!((two.len(), &*two[0].datagram), (1, &short(2, 10)[..]));
        assert_eq!((three.len(), &*three[0].datagram), (1, &short(3, 6)[..]));
        assert_eq!((three[0].meta.len, three[0].meta.stride), (6, 6));

        let mut buf = [short(1, 10), short(0, 10), short(0, 4)].concat();
        let mut meta = received(24, 10);
        router.route(0, &mut buf, &mut meta);
        assert_eq!(meta.len, 14, "a short last segment moves up behind the kept ones");
        assert_eq!(buf[..14], [short(0, 10), short(0, 4)].concat());
        assert_eq!(drain(&mut inboxes[1]).len(), 1);
    }
}

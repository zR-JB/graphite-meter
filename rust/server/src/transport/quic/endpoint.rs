//! HTTP/3 endpoints: on Linux one per two runtime threads on one port, else one, each holding budgeted buffers.

use super::{
    Http3, Listener,
    budget::{self, INCOMING_BYTES, INCOMING_TOTAL_BYTES, endpoint_bytes},
    shard::{Router, Shard, ShardSocket, cid_generator, route_in_kernel},
};
use crate::{
    app::App,
    config::{Limits, could_not},
    limits::Lease,
    log,
    transport::tls::Certificates,
};
use graphite_meter_net::Pool;
use noq::{AsyncUdpSocket, UdpSender, crypto::rustls::QuicServerConfig};
use std::{
    io,
    net::SocketAddr,
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};
use tokio::runtime::Handle;

/// The most endpoints: their reservations stay a small part of the default budget on hosts with many cores.
const MAX_SHARDS: usize = 16;
pub(crate) const OVERFLOW: &str = "QUIC endpoint buffer size overflow";

pub struct Endpoints {
    pub listeners: Vec<Listener>,
    pub http3: Http3,
    pub bytes: usize,
}

/// What binding needs; `fits` refuses endpoint bytes the budget does not cover beside every connection's floor.
pub struct Binding<'a, F> {
    pub app: &'a Arc<App>,
    pub certificates: &'a Arc<Certificates>,
    pub fits: F,
}

impl<F: Fn(usize) -> Result<(), String>> Binding<'_, F> {
    /// On Linux, endpoints on half of `pool`'s runtimes, 2 to 16 as budget covers (2 beat 1 or 4 on 4 workers).
    pub fn bind(&self, address: SocketAddr, pool: &Pool) -> Result<Endpoints, String> {
        let runtimes = pool.runtimes();
        if cfg!(target_os = "linux") && runtimes.len() > 1 {
            let wanted = (runtimes.len() / 2).clamp(2, MAX_SHARDS);
            let shards = self.shards(address, &runtimes[..wanted])?;
            let bound = shards
                .as_ref()
                .map_or(1, |endpoints| endpoints.listeners.len());
            if bound < wanted {
                log!(
                    Warn,
                    "memory",
                    "buffer budget covers {bound} of {wanted} QUIC endpoints; raise GM_MAX_BUFFER_BYTES for more"
                );
            }
            if let Some(endpoints) = shards {
                return Ok(endpoints);
            }
        }
        self.single(address, pool.next())
    }

    fn single(&self, address: SocketAddr, runtime: Handle) -> Result<Endpoints, String> {
        let udp = Udp::bind(address, 1, &runtime)?;
        let mut config = noq::EndpointConfig::default();
        config.cid_generator(cid_generator(0));
        let bytes = udp.bytes(&config, 1, &self.app.config().limits)?;
        (self.fits)(bytes)?;
        let http3 = self.http3(1)?;
        let listener = self.endpoint(
            config,
            &http3.config,
            (udp.socket, udp.quic),
            bytes,
            runtime,
            None,
        )?;
        Ok(Endpoints {
            listeners: vec![listener],
            http3,
            bytes,
        })
    }

    /// Endpoints on as many `runtimes` as the budget covers, sharing keys and limits; `None` under two.
    fn shards(
        &self,
        address: SocketAddr,
        runtimes: &[Handle],
    ) -> Result<Option<Endpoints>, String> {
        let config = noq::EndpointConfig::default();
        let first = Udp::bind(address, runtimes.len(), &runtimes[0])?;
        // The others join the port the first bound; it stands for them until they exist.
        let address = first
            .socket
            .local_addr()
            .map_err(|error| error.to_string())?;
        let total = |shards| {
            first
                .bytes(&config, shards, &self.app.config().limits)
                .ok()?
                .checked_mul(shards)
        };
        let fitting = (2..=runtimes.len())
            .rev()
            .find(|&shards| total(shards).is_some_and(|total| (self.fits)(total).is_ok()));
        let Some(shards) = fitting else {
            return Ok(None);
        };
        let mut sockets = vec![first];
        for runtime in &runtimes[1..shards] {
            sockets.push(Udp::bind(address, runtimes.len(), runtime)?);
        }
        let each = sockets
            .iter()
            .map(|udp| udp.bytes(&config, shards, &self.app.config().limits))
            .collect::<Result<Vec<_>, _>>()?;
        let bytes = each
            .iter()
            .try_fold(0_usize, |total, &bytes| total.checked_add(bytes))
            .ok_or(OVERFLOW)?;
        (self.fits)(bytes)?;
        let http3 = self.http3(shards)?;
        let refused = sockets.iter().find_map(|udp| udp.routed.as_ref().err());
        if let Some(error) = refused {
            log!(
                Warn,
                "udp",
                "kernel routing by connection ID unavailable: {error}; QUIC endpoints route in user space and \
                 connections stay where they arrive"
            );
        }
        let packet = budget::packet_bytes(&config).ok_or(OVERFLOW)?;
        let (router, inboxes) = Router::new(shards, packet, refused.is_none());
        let mut listeners = Vec::with_capacity(shards);
        for (shard, ((udp, bytes), inbox)) in sockets.into_iter().zip(each).zip(inboxes).enumerate()
        {
            let socket = Box::new(ShardSocket::new(udp.socket, shard, router.clone(), inbox));
            let mut config = config.clone();
            config.cid_generator(cid_generator(
                u8::try_from(shard).map_err(|error| error.to_string())?,
            ));
            let (runtime, place) = (runtimes[shard].clone(), Some(router.shard(shard)));
            listeners.push(self.endpoint(
                config,
                &http3.config,
                (socket, udp.quic),
                bytes,
                runtime,
                place,
            )?);
        }
        Ok(Some(Endpoints {
            listeners,
            http3,
            bytes,
        }))
    }

    /// An endpoint on `runtime`, its socket holding `bytes` of the budget till dropped; `shard` counts its connections.
    fn endpoint(
        &self,
        endpoint: noq::EndpointConfig,
        config: &noq::ServerConfig,
        (socket, quic): (Box<dyn AsyncUdpSocket>, Arc<dyn noq::Runtime>),
        bytes: usize,
        runtime: Handle,
        shard: Option<Shard>,
    ) -> Result<Listener, String> {
        let lease = self.app.budget().reserve(bytes);
        let lease = lease.ok_or("the buffer budget cannot cover the QUIC endpoint buffers")?;
        let socket = Box::new(Budgeted {
            socket,
            lease: Arc::new(lease),
        });
        let _entered = runtime.enter();
        let endpoint =
            noq::Endpoint::new_with_abstract_socket(endpoint, Some(config.clone()), socket, quic);
        let endpoint = endpoint.map_err(|error| error.to_string())?;
        Ok(Listener {
            endpoint,
            app: self.app.clone(),
            runtime,
            shard,
        })
    }

    /// What a shard's connections share: their part of server-wide incoming limits; handshakes queue per endpoint.
    fn http3(&self, shards: usize) -> Result<Http3, String> {
        let crypto = QuicServerConfig::try_from(self.certificates.server_config(b"h3"));
        let mut config =
            noq::ServerConfig::with_crypto(Arc::new(crypto.map_err(|error| error.to_string())?));
        config
            .max_incoming(self.app.config().limits.connections.get().div_ceil(shards))
            .incoming_buffer_size(INCOMING_BYTES)
            .incoming_buffer_size_total(INCOMING_TOTAL_BYTES.div_ceil(shards as u64));
        let mut transport = budget::transport(&self.app.config().limits)?;
        transport.shared_budget(Some(self.app.budget().noq()));
        config.transport_config(Arc::new(transport));
        Ok(Http3 {
            app: self.app.clone(),
            config,
            certificates: self.certificates.clone(),
            max_requests: budget::max_requests(&self.app.config().limits)? as usize,
        })
    }
}

/// One of `sockets` UDP sockets wrapped for noq, with its kernel bytes and whether the kernel took the routing program.
struct Udp {
    socket: Box<dyn AsyncUdpSocket>,
    quic: Arc<dyn noq::Runtime>,
    kernel: usize,
    routed: io::Result<()>,
}

impl Udp {
    fn bind(address: SocketAddr, sockets: usize, runtime: &Handle) -> Result<Self, String> {
        let _entered = runtime.enter();
        let bound = graphite_meter_net::bind_udp(address, sockets);
        let (socket, shortfall) = bound
            .map_err(|error| could_not(&format!("listen on {address}/udp (GM_H3_ADDR)"), &error))?;
        static WARNED: AtomicBool = AtomicBool::new(false);
        if let Some(shortfall) = shortfall.filter(|_| !WARNED.swap(true, Ordering::Relaxed)) {
            log!(
                Warn,
                "udp",
                "UDP buffer too small for QUIC above about 1 Gbit/s: {shortfall}; raise net.core.rmem_max and \
                 net.core.wmem_max on the host (docs/DEPLOYMENT.md, UDP buffers)"
            );
        }
        let buffers = socket2::SockRef::from(&socket);
        let kernel = buffers
            .recv_buffer_size()
            .and_then(|receive| Ok(receive + buffers.send_buffer_size()?));
        let kernel = kernel.map_err(|error| error.to_string())?;
        let routed = if sockets > 1 {
            route_in_kernel(&socket)
        } else {
            Ok(())
        };
        let quic = noq::default_runtime().ok_or("no async runtime for QUIC")?;
        let socket = quic
            .wrap_udp_socket(socket)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            socket,
            quic,
            kernel,
            routed,
        })
    }

    fn bytes(
        &self,
        config: &noq::EndpointConfig,
        shards: usize,
        limits: &Limits,
    ) -> Result<usize, String> {
        let segments = self.socket.max_receive_segments().get();
        endpoint_bytes(
            config,
            shards,
            limits.connections.get(),
            self.kernel,
            segments,
        )
        .ok_or_else(|| OVERFLOW.into())
    }
}

/// An endpoint's socket, whose buffers hold their reservation until the socket and its senders drop.
#[derive(Debug)]
struct Budgeted {
    socket: Box<dyn AsyncUdpSocket>,
    lease: Arc<Lease>,
}

impl AsyncUdpSocket for Budgeted {
    fn create_sender(&self) -> Pin<Box<dyn UdpSender>> {
        let sender = self.socket.create_sender();
        Box::pin(BudgetedSender {
            sender,
            _lease: self.lease.clone(),
        })
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        buffers: &mut [io::IoSliceMut<'_>],
        meta: &mut [noq::udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.socket.poll_recv(cx, buffers, meta)
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

#[derive(Debug)]
struct BudgetedSender {
    sender: Pin<Box<dyn UdpSender>>,
    _lease: Arc<Lease>,
}

impl UdpSender for BudgetedSender {
    fn poll_send(
        mut self: Pin<&mut Self>,
        transmit: &noq::udp::Transmit<'_>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        self.sender.as_mut().poll_send(transmit, cx)
    }

    fn max_transmit_segments(&self) -> NonZeroUsize {
        self.sender.max_transmit_segments()
    }
}

//! Binding HTTP/3's endpoints: on Linux one per two runtime threads on a shared port, elsewhere one, each holding its
//! buffers from the budget while its socket lives.

use super::{
    Http3, Listener,
    budget::{self, INCOMING_BYTES, INCOMING_TOTAL_BYTES, endpoint_bytes},
    shard::{Router, ShardSocket, cid_generator},
};
use crate::{
    app::App,
    config::{Limits, path_error},
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
    sync::Arc,
    task::{Context, Poll},
};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

/// The most endpoints: their reservations stay a small part of the default budget on hosts with many cores.
const MAX_SHARDS: usize = 16;
const OVERFLOW: &str = "QUIC endpoint buffer size overflow";

/// HTTP/3's endpoints on one port and what their connections share.
pub struct Endpoints {
    pub listeners: Vec<Listener>,
    pub http3: Http3,
    /// What their buffers hold of the budget.
    pub bytes: usize,
}

/// What binding needs; `fits` refuses endpoint bytes the budget does not cover beside every connection's floor.
pub struct Binding<'a, F> {
    pub app: &'a Arc<App>,
    pub limits: &'a Limits,
    pub certificates: &'a Arc<Certificates>,
    pub fits: F,
    pub shutdown: &'a CancellationToken,
}

impl<F: Fn(usize) -> Result<(), String>> Binding<'_, F> {
    /// On Linux, endpoints on half of `pool`'s runtimes, two to sixteen, as many as the budget covers: two cost less
    /// CPU per byte than one or four on four workers. Otherwise, or when fewer than two fit, one endpoint.
    pub fn bind(&self, address: SocketAddr, pool: &Pool) -> Result<Endpoints, String> {
        let runtimes = pool.runtimes();
        if cfg!(target_os = "linux") && runtimes.len() > 1 {
            let wanted = (runtimes.len() / 2).clamp(2, MAX_SHARDS);
            let shards = self.shards(address, &runtimes[..wanted])?;
            let bound = shards.as_ref().map_or(1, |endpoints| endpoints.listeners.len());
            if bound < wanted {
                log!("[gm:memory] the buffer budget covers {bound} of {wanted} QUIC endpoints");
            }
            if let Some(endpoints) = shards {
                return Ok(endpoints);
            }
        }
        self.single(address, pool.next())
    }

    /// One endpoint on `runtime`.
    fn single(&self, address: SocketAddr, runtime: Handle) -> Result<Endpoints, String> {
        let udp = Udp::bind(address, 1, &runtime)?;
        let config = noq::EndpointConfig::default();
        let bytes = udp.bytes(&config, 1, self.limits)?;
        (self.fits)(bytes)?;
        let http3 = self.http3(1)?;
        let listener = self.endpoint(config, &http3.config, udp.socket, udp.quic, bytes, runtime)?;
        Ok(Endpoints { listeners: vec![listener], http3, bytes })
    }

    /// Endpoints on as many of `runtimes` as the budget covers, each socket bound and endpoint built on its runtime,
    /// sharing one token key, reset key and every server-wide limit; `None` when fewer than two fit.
    fn shards(&self, address: SocketAddr, runtimes: &[Handle]) -> Result<Option<Endpoints>, String> {
        let config = noq::EndpointConfig::default();
        let first = Udp::bind(address, runtimes.len(), &runtimes[0])?;
        // The others join the port the first bound; it stands for them until they exist.
        let address = first.socket.local_addr().map_err(|error| error.to_string())?;
        let total = |shards| first.bytes(&config, shards, self.limits).ok()?.checked_mul(shards);
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
            .map(|udp| udp.bytes(&config, shards, self.limits))
            .collect::<Result<Vec<_>, _>>()?;
        let bytes = each
            .iter()
            .try_fold(0_usize, |total, &bytes| total.checked_add(bytes))
            .ok_or(OVERFLOW)?;
        (self.fits)(bytes)?;
        let http3 = self.http3(shards)?;
        let (router, inboxes) = Router::new(shards, budget::packet_bytes(&config).ok_or(OVERFLOW)?);
        let mut listeners = Vec::with_capacity(shards);
        for (shard, ((udp, bytes), inbox)) in sockets.into_iter().zip(each).zip(inboxes).enumerate() {
            let socket = Box::new(ShardSocket::new(udp.socket, shard, router.clone(), inbox));
            let mut config = config.clone();
            config.cid_generator(cid_generator(u8::try_from(shard).map_err(|error| error.to_string())?));
            let runtime = runtimes[shard].clone();
            listeners.push(self.endpoint(config, &http3.config, socket, udp.quic, bytes, runtime)?);
        }
        Ok(Some(Endpoints { listeners, http3, bytes }))
    }

    /// An endpoint built on `runtime`, whose socket holds `bytes` of the budget until it and its senders drop.
    fn endpoint(
        &self,
        endpoint: noq::EndpointConfig,
        config: &noq::ServerConfig,
        socket: Box<dyn AsyncUdpSocket>,
        quic: Arc<dyn noq::Runtime>,
        bytes: usize,
        runtime: Handle,
    ) -> Result<Listener, String> {
        let lease = self.app.budget().reserve(bytes);
        let lease = lease.ok_or("the buffer budget cannot cover the QUIC endpoint buffers")?;
        let socket = Box::new(Budgeted { socket, lease: Arc::new(lease) });
        let _entered = runtime.enter();
        let endpoint = noq::Endpoint::new_with_abstract_socket(endpoint, Some(config.clone()), socket, quic);
        let endpoint = endpoint.map_err(|error| error.to_string())?;
        Ok(Listener { endpoint, app: self.app.clone(), runtime })
    }

    /// What the connections of one of `shards` endpoints share: each admits its part of the server-wide incoming
    /// limits, while every handshake keeps its own queue on the endpoint its packets reach.
    fn http3(&self, shards: usize) -> Result<Http3, String> {
        let crypto = QuicServerConfig::try_from(self.certificates.server_config(b"h3"));
        let mut config = noq::ServerConfig::with_crypto(Arc::new(crypto.map_err(|error| error.to_string())?));
        config
            .max_incoming(self.limits.connections.div_ceil(shards))
            .incoming_buffer_size(INCOMING_BYTES)
            .incoming_buffer_size_total(INCOMING_TOTAL_BYTES.div_ceil(shards as u64));
        let mut transport = budget::transport(self.limits);
        transport.shared_budget(Some(self.app.budget().noq()));
        config.transport_config(Arc::new(transport));
        Ok(Http3 {
            app: self.app.clone(),
            config,
            certificates: self.certificates.clone(),
            max_requests: budget::max_requests(self.limits) as usize,
            shutdown: self.shutdown.clone(),
        })
    }
}

/// One of `sockets` UDP sockets on an address, wrapped for noq on the runtime it was bound on, with the kernel buffer
/// bytes it holds.
struct Udp {
    socket: Box<dyn AsyncUdpSocket>,
    quic: Arc<dyn noq::Runtime>,
    kernel: usize,
}

impl Udp {
    fn bind(address: SocketAddr, sockets: usize, runtime: &Handle) -> Result<Self, String> {
        let _entered = runtime.enter();
        let bound = graphite_meter_net::bind_udp(address, sockets);
        let (socket, warning) = bound.map_err(|error| path_error("listen udp", &address.to_string(), &error))?;
        if let Some(warning) = warning {
            log!("{warning}");
        }
        let buffers = socket2::SockRef::from(&socket);
        let kernel = buffers
            .recv_buffer_size()
            .and_then(|receive| Ok(receive + buffers.send_buffer_size()?));
        let kernel = kernel.map_err(|error| error.to_string())?;
        let quic = noq::default_runtime().ok_or("no async runtime for QUIC")?;
        let socket = quic.wrap_udp_socket(socket).map_err(|error| error.to_string())?;
        Ok(Self { socket, quic, kernel })
    }

    /// What one of `shards` endpoints on this socket holds of the budget.
    fn bytes(&self, config: &noq::EndpointConfig, shards: usize, limits: &Limits) -> Result<usize, String> {
        let segments = self.socket.max_receive_segments().get();
        endpoint_bytes(config, shards, limits.connections, self.kernel, segments).ok_or_else(|| OVERFLOW.into())
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
        Box::pin(BudgetedSender { sender, _lease: self.lease.clone() })
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

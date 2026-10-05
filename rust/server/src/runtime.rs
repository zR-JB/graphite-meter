//! The running server: its certificate, listeners bound before any serves, connections on the pinned runtimes, the
//! budget's terms, and the shutdown order (listeners close at once, lanes end, connections drain).

use crate::{
    app::{App, Endpoint},
    config::{Config, ENGINE_VERSION, ListenerKind, path_error},
    engine::download::BLOCK_BYTES,
    limits::{Budget, Transport},
    log,
    transport::{
        accept::{self, Listen},
        http1::Http1,
        http2::{self, Http2},
        quic::{self, Http3},
        tls::{self, Certificates},
    },
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_net::Pool;
use std::{
    future::Future,
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};
use tokio::{net::TcpListener, runtime::Handle};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

/// A server with its listeners bound.
pub struct Server {
    app: Arc<App>,
    pool: Pool,
    listeners: Vec<Listening>,
    certificates: Option<Arc<Certificates>>,
    shutdown: CancellationToken,
    auth: bool,
}

struct Listening {
    endpoint: Endpoint,
    /// The address as configured, which the startup line names.
    address: String,
    socket: Socket,
}

/// A listener's socket and what its connections speak, over TLS with the acceptor's ALPN.
enum Socket {
    Http1(TcpListener, Option<TlsAcceptor>),
    Http2(TcpListener, TlsAcceptor),
    Quic(Box<(quic::Listener, Http3)>),
}

impl Socket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Http1(socket, _) | Self::Http2(socket, _) => socket.local_addr(),
            Self::Quic(quic) => quic.0.local_addr(),
        }
    }
}

type Service<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

impl Server {
    /// Loads the certificate and binds every enabled listener, HTTP/3 on UDP beside its TCP companion's port; a
    /// failure closes those bound before it.
    pub async fn bind(mut config: Config) -> Result<Self, String> {
        let (tls, hosts, auth) = (config.tls.clone(), tls::covered_hosts(&config), config.auth.is_some());
        let terms = Terms::of(&config);
        let endpoint_bytes = Arc::new(AtomicUsize::new(configured_endpoint(&config)?));
        let endpoint = endpoint_bytes.clone();
        // Only QUIC floors grow with the chain.
        let fits = move |handshake| match terms.quic {
            Some(_) => terms.check(handshake, endpoint.load(Ordering::Relaxed)),
            None => Ok(()),
        };
        let certificates = match tls {
            Some(files) => {
                let load = move || Certificates::load(files, hosts, SystemTime::now(), fits);
                let load = tokio::task::spawn_blocking(load);
                Some(Arc::new(load.await.map_err(|error| error.to_string())??))
            }
            None => None,
        };
        let mut listeners = bind_tcp(&mut config, certificates.as_ref()).await?;
        let limits = config.limits;
        let shutdown = CancellationToken::new();
        let app = Arc::new(App::new(config, shutdown.clone())?);
        let pool = Pool::new().map_err(|error| format!("runtime threads: {error}"))?;
        let companion = listeners
            .iter()
            .find(|listening| listening.endpoint == Endpoint::H3Companion);
        if let (Some(companion), Some(certificates)) = (companion, &certificates) {
            let covered = |bytes| {
                terms.check(certificates.handshake_bytes(), bytes)?;
                endpoint_bytes.store(bytes, Ordering::Relaxed);
                Ok(())
            };
            let local = companion.socket.local_addr().map_err(|error| error.to_string())?;
            let bound = quic::bind(local, pool.next(), &app, &limits, certificates, covered, &shutdown)?;
            let (endpoint, address) = (Endpoint::Quic, companion.address.clone());
            listeners.push(Listening { endpoint, address, socket: Socket::Quic(Box::new(bound)) });
        }
        Ok(Self { app, pool, listeners, certificates, shutdown, auth })
    }

    /// Where the listener of `endpoint` accepts.
    pub fn local_addr(&self, endpoint: Endpoint) -> Option<SocketAddr> {
        let listening = self.listeners.iter().find(|listening| listening.endpoint == endpoint)?;
        listening.socket.local_addr().ok()
    }

    /// The buffer budget every listener draws on, for observation.
    pub fn budget(&self) -> Budget {
        self.app.budget().clone()
    }

    /// Serves until `stop`, then closes every listener at once, ends running lanes and drains connections.
    pub async fn serve(self, stop: impl Future<Output = ()>) -> Result<(), String> {
        let Self { app, pool, listeners, certificates, shutdown, auth } = self;
        let (pool, stopping) = (&pool, &shutdown);
        let mut services = FuturesUnordered::<Service<'_>>::new();
        for Listening { endpoint, address, socket } in listeners {
            let role = endpoint.role(auth);
            let (app, shutdown, next) = (app.clone(), shutdown.clone(), || pool.next());
            let (service, protocol) = match socket {
                Socket::Http1(socket, tls) => {
                    let http1 = Http1 { app: app.clone(), endpoint, tls, shutdown };
                    let serve = move |socket, peer| http1.connection(socket, peer);
                    (listen(app, socket, next, stopping, role, Transport::Tcp, serve), "tcp")
                }
                Socket::Http2(socket, tls) => {
                    let http2 = Http2 { app: app.clone(), tls, shutdown };
                    let serve = move |socket, peer| http2.connection(socket, peer);
                    (listen(app, socket, next, stopping, role, Transport::Tcp, serve), "tcp")
                }
                Socket::Quic(quic) => {
                    let (listener, http3) = *quic;
                    let runtime = listener.runtime();
                    let serve = move |incoming, peer| http3.connection(incoming, peer);
                    let next = move || runtime.clone();
                    (listen(app, listener, next, stopping, role, Transport::Quic, serve), "udp")
                }
            };
            log!("graphite-meter {ENGINE_VERSION} listening on {address}/{protocol} ({role})");
            services.push(service);
        }
        if let Some(certificates) = certificates {
            services.push(Box::pin(async move {
                certificates.watch(stopping.clone()).await;
                Ok(())
            }));
        }
        let mut result = tokio::select! {
            () = stop => Ok(()),
            Some(ended) = services.next() => ended.and(Err("server listener stopped unexpectedly".into())),
        };
        shutdown.cancel();
        while let Some(ended) = services.next().await {
            result = result.and(ended);
        }
        result
    }
}

/// Binds every enabled listener's TCP socket, HTTP/3's for its companion, and names the ports they bound in `config`.
async fn bind_tcp(config: &mut Config, certificates: Option<&Arc<Certificates>>) -> Result<Vec<Listening>, String> {
    let mut listeners = Vec::new();
    for listener in &mut config.listeners {
        let (endpoint, alpn): (_, Option<&[u8]>) = match listener.kind {
            ListenerKind::H1 => (Endpoint::H1, None),
            ListenerKind::H1Tls => (Endpoint::H1Tls, Some(b"http/1.1")),
            ListenerKind::H2 => (Endpoint::H2, Some(b"h2")),
            ListenerKind::H3 => (Endpoint::H3Companion, Some(b"http/1.1")),
        };
        let tls = match (alpn, certificates) {
            (None, _) => None,
            (Some(alpn), Some(certificates)) => Some(certificates.acceptor(alpn)),
            (Some(_), None) => return Err("TLS listeners need a certificate".into()),
        };
        let configured = &mut listener.address;
        let socket = bind(configured)
            .await
            .map_err(|error| path_error("listen tcp", configured, &error))?;
        let address = configured.clone();
        if let Ok(local) = socket.local_addr() {
            *configured = bound(configured, local.port());
        }
        let socket = match (endpoint, tls) {
            (Endpoint::H2, Some(tls)) => Socket::Http2(socket, tls),
            (_, tls) => Socket::Http1(socket, tls),
        };
        listeners.push(Listening { endpoint, address, socket });
    }
    Ok(listeners)
}

/// Accepts on `socket` until `stopping`, each connection holding its share and running on a runtime `runtime` names,
/// then drains.
fn listen<'a, L, F>(
    app: Arc<App>,
    socket: L,
    runtime: impl Fn() -> Handle + Send + 'a,
    stopping: &'a CancellationToken,
    role: &'static str,
    transport: Transport,
    serve: impl Fn(L::Connection, SocketAddr) -> F + Send + 'a,
) -> Service<'a>
where
    L: Listen + Send + 'a,
    F: Future<Output = ()> + Send + 'static,
{
    Box::pin(async move {
        let hold = |peer: SocketAddr| app.connection(peer.ip(), transport);
        let serving = accept::serve(socket, runtime, stopping, hold, serve);
        serving.await.map_err(|error| format!("{role}: {error}"))
    })
}

/// Refuses a buffer budget below every connection's floor, the QUIC endpoint's buffers before its socket exists and
/// the download block.
pub fn check_budget(config: &Config) -> Result<(), String> {
    Terms::of(config).check(0, configured_endpoint(config)?)
}

/// The QUIC endpoint's buffers without its socket's, or none without HTTP/3.
fn configured_endpoint(config: &Config) -> Result<usize, String> {
    match config.listener(ListenerKind::H3) {
        Some(_) => quic::endpoint_bytes(&noq::EndpointConfig::default(), config.limits.connections, 0, 1)
            .ok_or_else(|| "QUIC endpoint buffer size overflow".into()),
        None => Ok(0),
    }
}

/// What the buffer budget must cover: every connection's floor beside the QUIC endpoint's buffers and the download
/// block.
#[derive(Debug, Clone, Copy)]
struct Terms {
    limit: usize,
    connections: usize,
    h2: bool,
    /// noq's own floor per connection, with HTTP/3 enabled.
    quic: Option<usize>,
}

impl Terms {
    fn of(config: &Config) -> Self {
        Self {
            limit: config.max_buffer_bytes,
            connections: config.limits.connections,
            h2: config.listener(ListenerKind::H2).is_some(),
            quic: config
                .listener(ListenerKind::H3)
                .map(|_| quic::noq_floor(&config.limits)),
        }
    }

    /// Refuses a budget that a QUIC handshake of `handshake` bytes and endpoint buffers of `endpoint` bytes leave
    /// short.
    fn check(&self, handshake: usize, endpoint: usize) -> Result<(), String> {
        let quic = self
            .quic
            .map_or(0, |noq| quic::floor_bytes(handshake).saturating_add(noq));
        let floor = quic.max(if self.h2 { http2::FLOOR_BYTES } else { 0 });
        let (limit, connections) = (self.limit, self.connections);
        let minimum = floor as u128 * connections as u128 + endpoint as u128 + BLOCK_BYTES as u128;
        if minimum > limit as u128 {
            return Err(format!(
                "GM_MAX_BUFFER_BYTES ({limit}) must be at least {minimum}: GM_MAX_CONNECTIONS ({connections}) \
                 connection floors of {floor} bytes, {endpoint} bytes of QUIC endpoint buffers and the \
                 {BLOCK_BYTES}-byte download block"
            ));
        }
        Ok(())
    }
}

/// Completes at SIGINT or SIGTERM; it listens from its call, so a signal during startup stops the server once it
/// serves.
pub fn stop_signal() -> io::Result<impl Future<Output = ()>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (mut interrupt, mut terminate) = (signal(SignalKind::interrupt())?, signal(SignalKind::terminate())?);
        Ok(async move {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
        })
    }
    #[cfg(not(unix))]
    Ok(async {
        let _ = tokio::signal::ctrl_c().await;
    })
}

/// `address` with a port 0 replaced by the `port` it bound.
fn bound(address: &str, port: u16) -> String {
    match address.rsplit_once(':') {
        Some((host, "0")) => format!("{host}:{port}"),
        _ => address.into(),
    }
}

/// Binds `address`; a bare `:port` takes every IPv6 and IPv4 address, or every IPv4 one on a host without IPv6.
async fn bind(address: &str) -> io::Result<TcpListener> {
    let Some(port) = address.strip_prefix(':') else {
        return TcpListener::bind(address).await;
    };
    let port: u16 = port
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid port"))?;
    let ipv4 = || TcpListener::bind((Ipv4Addr::UNSPECIFIED, port));
    let dual = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, None).and_then(|socket| {
        socket.set_only_v6(false)?;
        Ok(socket)
    });
    let Ok(socket) = dual else {
        return ipv4().await;
    };
    #[cfg(unix)]
    socket.set_reuse_address(true)?;
    match socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into()) {
        Err(error) if matches!(error.kind(), io::ErrorKind::AddrNotAvailable | io::ErrorKind::Unsupported) => {
            ipv4().await
        }
        Err(error) => Err(error),
        Ok(()) => {
            socket.listen(1024)?;
            socket.set_nonblocking(true)?;
            TcpListener::from_std(socket.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{self, Loaded};
    use std::ffi::OsString;

    fn config(env: &[(&str, &str)]) -> Config {
        let lookup = |name: &str| env.iter().find(|(key, _)| *key == name).map(|(_, value)| value.into());
        match config::load(lookup, Vec::<OsString>::new(), &mut Vec::new()) {
            Ok(Loaded::Config(config)) => *config,
            other => panic!("{other:?}"),
        }
    }

    const TLS: [(&str, &str); 2] = [("GM_TLS_CERT", "/cert.pem"), ("GM_TLS_KEY", "/key.pem")];

    #[test]
    fn the_default_budget_covers_every_connection_with_every_listener_enabled() {
        let listeners = [("GM_H1_TLS_ADDR", ":7247"), ("GM_H2_ADDR", ":7248"), ("GM_H3_ADDR", ":7249")];
        let every = [&TLS[..], &listeners[..]].concat();
        assert_eq!(check_budget(&config(&every)), Ok(()));
        let totals = [
            ("GM_MAX_CONNECTIONS_PER_CLIENT", "4096"),
            ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "256"),
            ("GM_MAX_SESSIONS_PER_CLIENT", "64"),
        ];
        let totals = config(&[&every[..], &totals[..]].concat());
        assert_eq!(check_budget(&totals), Ok(()));
        let small = [("GM_MAX_BUFFER_BYTES", "262143")];
        assert!(check_budget(&config(&small)).is_err(), "the download block needs its bytes");
    }

    #[test]
    fn http3_adds_its_connection_floor_handshake_and_endpoint_buffers() {
        let env = [
            ("GM_H3_ADDR", ":7249"),
            ("GM_MAX_CONNECTIONS", "2"),
            ("GM_MAX_CONNECTIONS_PER_CLIENT", "2"),
        ];
        let config = config(&[&TLS[..], &env[..]].concat());
        assert_eq!(quic::noq_floor(&config.limits) >> 10, 481, "noq's stream floor at default limits");
        let endpoint = configured_endpoint(&config).unwrap();
        let floor = quic::floor_bytes(0) + quic::noq_floor(&config.limits);
        let terms = Terms {
            limit: 2 * floor + endpoint + BLOCK_BYTES,
            ..Terms::of(&config)
        };
        assert_eq!(terms.check(0, endpoint), Ok(()));
        let refused = terms.check(1, endpoint).unwrap_err();
        let minimum = terms.limit + 2;
        let message = format!(
            "GM_MAX_BUFFER_BYTES ({}) must be at least {minimum}: GM_MAX_CONNECTIONS (2) connection floors of {} \
             bytes, {endpoint} bytes of QUIC endpoint buffers and the 262144-byte download block",
            terms.limit,
            floor + 1
        );
        assert_eq!(refused, message, "a handshake byte more on each connection");
        assert!(terms.check(0, endpoint + 1).is_err(), "the endpoint's socket buffers count");
    }
}

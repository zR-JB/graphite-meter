//! The running server: its certificate, listeners bound before any serves, connections on the pinned runtimes, and
//! the shutdown order (listeners close at once, lanes end, connections drain).

use crate::{
    app::{App, Endpoint},
    config::{Config, ENGINE_VERSION, ListenerKind, path_error},
    limits::Transport,
    log,
    transport::{
        accept,
        http1::Http1,
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
    sync::Arc,
    time::SystemTime,
};
use tokio::net::TcpListener;
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
    socket: TcpListener,
}

type Service<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

impl Server {
    /// Loads the certificate and binds every enabled listener; a failure closes those bound before it.
    pub async fn bind(config: Config) -> Result<Self, String> {
        let served = config.listeners.iter().map(|listener| {
            let endpoint = match listener.kind {
                ListenerKind::H1 => Endpoint::H1,
                ListenerKind::H1Tls => Endpoint::H1Tls,
                kind @ (ListenerKind::H2 | ListenerKind::H3) => {
                    return Err(format!("{} listeners are unavailable in this build", kind.name()));
                }
            };
            Ok((endpoint, listener.address.clone()))
        });
        let served = served.collect::<Result<Vec<_>, String>>()?;
        let (tls, hosts, auth) = (config.tls.clone(), tls::covered_hosts(&config), config.auth.is_some());
        let shutdown = CancellationToken::new();
        let app = Arc::new(App::new(config, shutdown.clone())?);
        let certificates = match tls {
            Some(files) => {
                let load = tokio::task::spawn_blocking(move || Certificates::load(files, hosts, SystemTime::now()));
                Some(Arc::new(load.await.map_err(|error| error.to_string())??))
            }
            None => None,
        };
        let mut listeners = Vec::new();
        for (endpoint, address) in served {
            let socket = bind(&address)
                .await
                .map_err(|error| path_error("listen tcp", &address, &error))?;
            listeners.push(Listening { endpoint, address, socket });
        }
        let pool = Pool::new().map_err(|error| format!("runtime threads: {error}"))?;
        Ok(Self { app, pool, listeners, certificates, shutdown, auth })
    }

    /// Where the listener of `endpoint` accepts.
    pub fn local_addr(&self, endpoint: Endpoint) -> Option<SocketAddr> {
        let listening = self.listeners.iter().find(|listening| listening.endpoint == endpoint)?;
        listening.socket.local_addr().ok()
    }

    /// Serves until `stop`, then closes every listener at once, ends running lanes and drains connections.
    pub async fn serve(self, stop: impl Future<Output = ()>) -> Result<(), String> {
        let Self { app, pool, listeners, certificates, shutdown, auth } = self;
        let (pool, stopping) = (&pool, &shutdown);
        let mut services = FuturesUnordered::<Service<'_>>::new();
        for Listening { endpoint, address, socket } in listeners {
            let role = endpoint.role(auth);
            log!("graphite-meter {ENGINE_VERSION} listening on {address}/tcp ({role})");
            let tls = certificates.as_ref().filter(|_| endpoint == Endpoint::H1Tls);
            let http1 = Http1 {
                app: app.clone(),
                endpoint,
                tls: tls.map(|certificates| certificates.acceptor(b"http/1.1")),
                shutdown: shutdown.clone(),
            };
            let app = app.clone();
            services.push(Box::pin(async move {
                let hold = |peer: SocketAddr| app.connection(peer.ip(), Transport::Tcp);
                let serving =
                    accept::serve(socket, pool, stopping, hold, |socket, peer| http1.connection(socket, peer));
                serving.await.map_err(|error| format!("{role}: {error}"))
            }));
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

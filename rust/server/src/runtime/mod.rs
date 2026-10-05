//! The running server: its certificate, listeners bound before any serves, connections on the pinned runtimes, the
//! work beside them, and the shutdown order (listeners close at once, lanes end, connections drain).

mod background;
mod tcp;
mod terms;

pub use terms::check_budget;

use crate::{
    app::{App, Endpoint},
    config::{Config, ENGINE_VERSION, Methods},
    limits::{Budget, Transport},
    log,
    transport::{
        accept::{self, Listen},
        http1::Http1,
        http2::Http2,
        quic::{Binding, Endpoints},
        tls::{self, Certificates},
    },
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use graphite_meter_net::Pool;
use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};
use terms::{Terms, configured_endpoint};
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
    verbose: bool,
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
    Quic(Box<Endpoints>),
}

impl Socket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Http1(socket, _) | Self::Http2(socket, _) => socket.local_addr(),
            Self::Quic(quic) => quic.listeners[0].local_addr(),
        }
    }
}

type Service<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

impl Server {
    /// Loads the certificate and binds every enabled listener, HTTP/3 on UDP beside its TCP companion's port; a
    /// failure closes those bound before it.
    pub async fn bind(mut config: Config) -> Result<Self, String> {
        let (tls, hosts, auth) = (config.tls.clone(), tls::covered_hosts(&config), config.auth.is_some());
        let oidc_only = matches!(config.auth.as_ref().map(|auth| &auth.methods), Some(Methods::Oidc(_)));
        let terms = Terms::of(&config)?;
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
        let mut listeners = tcp::bind_all(&mut config, certificates.as_ref()).await?;
        let (limits, verbose) = (config.limits, config.verbose);
        let shutdown = CancellationToken::new();
        let app = Arc::new(App::new(config, shutdown.clone())?);
        if oidc_only {
            app.auth()
                .discover()
                .await
                .map_err(|error| format!("OIDC discovery: {error}"))?;
        }
        let pool = Pool::new().map_err(|error| format!("runtime threads: {error}"))?;
        let companion = listeners
            .iter()
            .find(|listening| listening.endpoint == Endpoint::H3Companion);
        if let (Some(companion), Some(certificates)) = (companion, &certificates) {
            let fits = |bytes| terms.check(certificates.handshake_bytes(), bytes);
            let binding = Binding {
                app: &app,
                limits: &limits,
                certificates,
                fits,
                shutdown: &shutdown,
            };
            let local = companion.socket.local_addr().map_err(|error| error.to_string())?;
            let bound = binding.bind(local, &pool)?;
            endpoint_bytes.store(bound.bytes, Ordering::Relaxed);
            let (endpoint, address) = (Endpoint::Quic, companion.address.clone());
            listeners.push(Listening { endpoint, address, socket: Socket::Quic(Box::new(bound)) });
        }
        Ok(Self { app, pool, listeners, certificates, shutdown, auth, verbose })
    }

    /// Where the listener of `endpoint` accepts.
    pub fn local_addr(&self, endpoint: Endpoint) -> Option<SocketAddr> {
        let listening = self.listeners.iter().find(|listening| listening.endpoint == endpoint)?;
        listening.socket.local_addr().ok()
    }

    /// An observation hook for tests: how many endpoints share the HTTP/3 port.
    pub fn quic_endpoints(&self) -> usize {
        let quic = self.listeners.iter().find_map(|listening| match &listening.socket {
            Socket::Quic(quic) => Some(quic.listeners.len()),
            _ => None,
        });
        quic.unwrap_or(0)
    }

    /// The state every listener shares.
    pub fn app(&self) -> Arc<App> {
        self.app.clone()
    }

    /// An observation hook for tests: the buffer budget every listener draws on.
    pub fn budget(&self) -> Budget {
        self.app.budget().clone()
    }

    /// Serves until `stop`, then closes every listener at once, ends running lanes and drains connections.
    pub async fn serve(self, stop: impl Future<Output = ()>) -> Result<(), String> {
        let Self { app, pool, listeners, certificates, shutdown, auth, verbose } = self;
        let (pool, stopping) = (&pool, &shutdown);
        let mut services = FuturesUnordered::<Service<'_>>::new();
        for Listening { endpoint, address, socket } in listeners {
            let role = endpoint.role(auth);
            let (app, shutdown, next) = (app.clone(), shutdown.clone(), || pool.next());
            let protocol = match socket {
                Socket::Http1(socket, tls) => {
                    let http1 = Http1 { app: app.clone(), endpoint, tls, shutdown };
                    let serve = move |socket, peer| http1.connection(socket, peer);
                    services.push(listen(app, socket, next, stopping, role, Transport::Tcp, serve));
                    "tcp"
                }
                Socket::Http2(socket, tls) => {
                    let http2 = Http2 { app: app.clone(), tls, shutdown };
                    let serve = move |socket, peer| http2.connection(socket, peer);
                    services.push(listen(app, socket, next, stopping, role, Transport::Tcp, serve));
                    "tcp"
                }
                Socket::Quic(quic) => {
                    // Each endpoint accepts, answers Retry and runs its connections on its own runtime.
                    for listener in quic.listeners {
                        let (runtime, http3) = (listener.runtime(), quic.http3.clone());
                        let (app, stopping, next) = (app.clone(), shutdown.clone(), runtime.clone());
                        let serve = move |incoming, peer| http3.connection(incoming, peer);
                        let serving = runtime.spawn(async move {
                            let next = move || next.clone();
                            listen(app, listener, next, &stopping, role, Transport::Quic, serve).await
                        });
                        services
                            .push(Box::pin(async move { serving.await.map_err(|error| format!("{role}: {error}"))? }));
                    }
                    "udp"
                }
            };
            log!("graphite-meter {ENGINE_VERSION} listening on {address}/{protocol} ({role})");
        }
        if let Some(certificates) = certificates {
            services.push(Box::pin(async move {
                certificates.watch(stopping.clone()).await;
                Ok(())
            }));
        }
        let release = background::release_when_idle(&app, || background::release_memory(pool));
        services.push(until_stopped(stopping, release));
        if let Some(security) = app.auth().security() {
            services.push(until_stopped(stopping, background::log_security(security)));
        }
        if let Some(discovery) = app.auth().background_discovery() {
            services.push(until_stopped(stopping, discovery));
        }
        if verbose {
            services.push(until_stopped(stopping, background::log_verbose(&app)));
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

/// Runs `work` until the server stops.
fn until_stopped<'a>(stopping: &'a CancellationToken, work: impl Future<Output = ()> + Send + 'a) -> Service<'a> {
    Box::pin(async move {
        tokio::select! {
            () = stopping.cancelled() => {}
            () = work => {}
        }
        Ok(())
    })
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

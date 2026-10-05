//! The running server: its certificate, listeners bound before any serves, connections on the pinned runtimes, and
//! the shutdown order (listeners close at once, lanes end, connections drain).

use crate::{
    app::{App, Endpoint},
    config::{Config, ENGINE_VERSION, ListenerKind, path_error},
    engine::download::BLOCK_BYTES,
    limits::Transport,
    log,
    transport::{
        accept,
        http1::Http1,
        http2::{self, Http2},
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
use tokio::net::{TcpListener, TcpStream};
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
    socket: TcpListener,
    protocol: Protocol,
}

/// What a listener's connections speak, over TLS with the acceptor's ALPN.
enum Protocol {
    Http1(Option<TlsAcceptor>),
    Http2(TlsAcceptor),
}

type Service<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

impl Server {
    /// Loads the certificate and binds every enabled listener; a failure closes those bound before it.
    pub async fn bind(mut config: Config) -> Result<Self, String> {
        let served = config.listeners.iter().enumerate().map(|(index, listener)| {
            let endpoint = match listener.kind {
                ListenerKind::H1 => Endpoint::H1,
                ListenerKind::H1Tls => Endpoint::H1Tls,
                ListenerKind::H2 => Endpoint::H2,
                kind @ ListenerKind::H3 => {
                    return Err(format!("{} listeners are unavailable in this build", kind.name()));
                }
            };
            Ok((endpoint, index))
        });
        let served = served.collect::<Result<Vec<_>, String>>()?;
        let (tls, hosts, auth) = (config.tls.clone(), tls::covered_hosts(&config), config.auth.is_some());
        let certificates = match tls {
            Some(files) => {
                let load = tokio::task::spawn_blocking(move || Certificates::load(files, hosts, SystemTime::now()));
                Some(Arc::new(load.await.map_err(|error| error.to_string())??))
            }
            None => None,
        };
        let mut listeners = Vec::new();
        for (endpoint, index) in served {
            let protocol = match (endpoint, &certificates) {
                (Endpoint::H1, _) => Protocol::Http1(None),
                (Endpoint::H1Tls, Some(certificates)) => Protocol::Http1(Some(certificates.acceptor(b"http/1.1"))),
                (Endpoint::H2, Some(certificates)) => Protocol::Http2(certificates.acceptor(b"h2")),
                _ => return Err("TLS listeners need a certificate".into()),
            };
            let configured = &mut config.listeners[index].address;
            let socket = bind(configured)
                .await
                .map_err(|error| path_error("listen tcp", configured, &error))?;
            let address = configured.clone();
            if let Ok(local) = socket.local_addr() {
                *configured = bound(configured, local.port());
            }
            listeners.push(Listening { endpoint, address, socket, protocol });
        }
        let shutdown = CancellationToken::new();
        let app = Arc::new(App::new(config, shutdown.clone())?);
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
        for Listening { endpoint, address, socket, protocol } in listeners {
            let role = endpoint.role(auth);
            log!("graphite-meter {ENGINE_VERSION} listening on {address}/tcp ({role})");
            let (app, shutdown) = (app.clone(), shutdown.clone());
            services.push(match protocol {
                Protocol::Http1(tls) => {
                    let http1 = Http1 { app: app.clone(), endpoint, tls, shutdown };
                    listen(app, socket, pool, stopping, role, move |socket, peer| http1.connection(socket, peer))
                }
                Protocol::Http2(tls) => {
                    let http2 = Http2 { app: app.clone(), tls, shutdown };
                    listen(app, socket, pool, stopping, role, move |socket, peer| http2.connection(socket, peer))
                }
            });
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

/// Accepts on `socket` until `stopping`, each connection holding its share, then drains.
fn listen<'a, F>(
    app: Arc<App>,
    socket: TcpListener,
    pool: &'a Pool,
    stopping: &'a CancellationToken,
    role: &'static str,
    serve: impl Fn(TcpStream, SocketAddr) -> F + Send + Sync + 'a,
) -> Service<'a>
where
    F: Future<Output = ()> + Send + 'static,
{
    Box::pin(async move {
        let hold = |peer: SocketAddr| app.connection(peer.ip(), Transport::Tcp);
        let serving = accept::serve(socket, pool, stopping, hold, serve);
        serving.await.map_err(|error| format!("{role}: {error}"))
    })
}

/// Refuses a buffer budget below every connection's floor and the download block.
pub fn check_budget(config: &Config) -> Result<(), String> {
    let floor = config.listener(ListenerKind::H2).map_or(0, |_| http2::FLOOR_BYTES);
    let (limit, connections) = (config.max_buffer_bytes, config.limits.connections);
    let minimum = floor as u128 * connections as u128 + BLOCK_BYTES as u128;
    if minimum > limit as u128 {
        return Err(format!(
            "GM_MAX_BUFFER_BYTES ({limit}) must be at least {minimum}: GM_MAX_CONNECTIONS ({connections}) connection \
             floors of {floor} bytes, 0 bytes of QUIC endpoint buffers and the {BLOCK_BYTES}-byte download block"
        ));
    }
    Ok(())
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

    #[test]
    fn the_default_budget_covers_every_connection_with_http2_enabled() {
        let tls = [("GM_TLS_CERT", "/cert.pem"), ("GM_TLS_KEY", "/key.pem"), ("GM_H2_ADDR", ":7248")];
        assert_eq!(check_budget(&config(&tls)), Ok(()));
        let totals = [
            ("GM_MAX_CONNECTIONS_PER_CLIENT", "4096"),
            ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "256"),
        ];
        assert_eq!(check_budget(&config(&[&tls[..], &totals[..]].concat())), Ok(()));
        let small = [("GM_MAX_BUFFER_BYTES", "262143")];
        assert!(check_budget(&config(&small)).is_err(), "the download block needs its bytes");
    }
}

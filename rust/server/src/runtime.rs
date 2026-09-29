//! Process-level ownership of listeners, connection threads, certificate renewal, and shutdown.

use crate::{
    ServerError,
    config::{AuthMode, NativeKind, ValidatedConfig},
    http::{HttpServer, QuicEndpoint, topology},
    tls::Certificates,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{net::TcpListener, sync::watch};

type Service = Pin<Box<dyn Future<Output = Result<(), ServerError>> + Send>>;

/// Verbose logs report transfer rates each second and admission, as Go's, each thirty.
const TRANSFER_LOG_INTERVAL: Duration = Duration::from_secs(1);
const ADMISSION_LOG_INTERVAL: Duration = Duration::from_secs(30);

/// Bind every configured socket before serving any request. A bind failure
/// drops all previously opened sockets. Every running service is owned here;
/// normal shutdown waits for their connection tasks and certificate reads.
pub async fn run(config: ValidatedConfig, shutdown: impl Future<Output = ()>) -> Result<(), ServerError> {
    let server = Arc::new(HttpServer::new(config)?);
    let config = server.config.clone();
    server.initialize_auth().await?;
    let tls = if [NativeKind::H1Tls, NativeKind::H2, NativeKind::H3]
        .into_iter()
        .any(|kind| !config.listener(kind).address.is_empty())
    {
        let server = server.clone();
        Some(Certificates::load(&config, SystemTime::now(), move |bytes| {
            server.cover_handshake(bytes)
        })?)
    } else {
        None
    };
    let mut listeners = Vec::new();
    let mut quic = None;
    for kind in NativeKind::ALL {
        let address = &config.listener(kind).address;
        if address.is_empty() {
            continue;
        }
        let listener = bind(address)
            .await
            .map_err(|error| format!("{} listener {address}: {error}", kind.name()))?;
        let identity = (kind != NativeKind::H1)
            .then(|| tls.as_ref().expect("TLS identity loaded").config())
            .transpose()?;
        if kind == NativeKind::H3 {
            // The bootstrap companion and QUIC endpoint share the actual port,
            // including when the caller asks the OS to allocate one with :0.
            let identity = identity.clone().expect("TLS identity");
            quic = Some(Quic::bind(&server, identity, listener.local_addr()?)?);
        }
        listeners.push((kind, listener, identity));
    }
    if listeners.is_empty() {
        return Err("at least one listener must be enabled".into());
    }

    let (stop, stopped) = watch::channel(false);
    let mut services = FuturesUnordered::<Service>::new();
    for (kind, listener, identity) in listeners {
        crate::log!(
            "graphite-meter {} listening on {}/tcp ({})",
            crate::config::ENGINE_VERSION,
            listener.local_addr()?,
            topology::tcp(kind, config.auth.mode != AuthMode::Off).role,
        );
        let serving = server
            .clone()
            .serve(kind, listener, identity, cancelled(stopped.clone()));
        services.push(Box::pin(serving));
    }
    if let Some(quic) = quic {
        crate::log!(
            "graphite-meter {} listening on {}/udp ({})",
            crate::config::ENGINE_VERSION,
            quic.local_addr()?,
            topology::QUIC.role,
        );
        services.extend(quic.serve(&server, &stopped));
    }
    if let Some(tls) = tls {
        let stopped = stopped.clone();
        services.push(Box::pin(async move {
            tls.watch(cancelled(stopped), |result| {
                if let Err(error) = result {
                    crate::log!("[gm:tls] renewal rejected; keeping last valid certificate: {error}");
                }
            })
            .await
        }));
    }
    if config.auth.mode != AuthMode::Off {
        let server = server.clone();
        let stopped = stopped.clone();
        services.push(Box::pin(async move {
            tokio::select! {
                _ = cancelled(stopped) => {},
                _ = server.security_log() => {},
            }
            Ok(())
        }));
    }
    if config.verbose {
        let server = server.clone();
        let stopped = stopped.clone();
        services.push(Box::pin(async move {
            let mut transfer_tick = tokio::time::interval(TRANSFER_LOG_INTERVAL);
            let mut admission_tick = tokio::time::interval(ADMISSION_LOG_INTERVAL);
            transfer_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            admission_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            transfer_tick.tick().await;
            admission_tick.tick().await;
            let mut last = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    _ = cancelled(stopped.clone()) => break,
                    now = transfer_tick.tick() => {
                        server.log_transfers(now.duration_since(last));
                        last = now;
                    }
                    _ = admission_tick.tick() => server.log_admission(),
                }
            }
            Ok(())
        }));
    }
    tokio::pin!(shutdown);
    let mut result = tokio::select! {
        _ = &mut shutdown => Ok(()),
        result = services.next() => match result {
            Some(Err(error)) => Err(error),
            _ => Err("server listener stopped unexpectedly".into()),
        },
    };
    stop.send_replace(true);
    while let Some(stopped) = services.next().await {
        if result.is_ok() {
            result = stopped;
        }
    }
    result
}

async fn cancelled(mut stopped: watch::Receiver<bool>) {
    let _ = stopped.wait_for(|value| *value).await;
}

/// Current-thread runtimes on threads of their own, one for each worker of the multi-thread runtime that makes them,
/// which QUIC shards and TCP connections run on, so that a connection's tasks stay on one thread. Their threads end
/// with the pool; a current-thread runtime makes none.
pub(crate) struct Pool {
    pub(crate) runtimes: Vec<tokio::runtime::Handle>,
    _running: watch::Sender<()>,
}

impl Pool {
    pub(crate) fn new() -> std::io::Result<Self> {
        let workers = tokio::runtime::Handle::try_current().map_or(1, |runtime| runtime.metrics().num_workers());
        let (running, ended) = watch::channel(());
        let runtimes = (0..if workers > 1 { workers } else { 0 })
            .map(|index| {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                let (handle, mut ended) = (runtime.handle().clone(), ended.clone());
                std::thread::Builder::new()
                    .name(format!("gm-worker-{index}"))
                    .spawn(move || runtime.block_on(ended.changed()))?;
                Ok(handle)
            })
            .collect::<std::io::Result<_>>()?;
        Ok(Self {
            runtimes,
            _running: running,
        })
    }
}

/// HTTP/3 on the caller's runtime, or on shards on half of the pool's runtimes.
pub(crate) enum Quic {
    Endpoint(QuicEndpoint),
    Shards {
        shards: Vec<(tokio::runtime::Handle, QuicEndpoint)>,
        /// Tests count the datagrams it forwarded.
        #[cfg(all(test, target_os = "linux"))]
        router: crate::quic_shard::Router,
    },
}

const MAX_QUIC_SHARDS: usize = 16;

impl Quic {
    /// Shards on half of the pool's runtimes, at least two and at most sixteen, as many as the buffer budget covers.
    /// With four workers, two shards cost less CPU per byte than one or four for both one fast client and eight paced
    /// ones: each additional shard splits a connection's ACKs over more sockets, and so its sends into smaller
    /// bursts. Each shard holds its socket buffers, receive batch and forwarding queue for as long as it
    /// runs, and one connection never spreads over several, so sixteen keep those a small part of the default budget
    /// on a host with many cores. The shards split quic-go's 7 MiB socket buffers, each keeping 2 MiB at least, so
    /// the server's own queue stays near quic-go's single socket. Only Linux spreads unicast datagrams over
    /// `SO_REUSEPORT` sockets, so other targets keep one endpoint on this runtime.
    pub(crate) fn bind(
        server: &HttpServer,
        tls: Arc<rustls::ServerConfig>,
        address: SocketAddr,
    ) -> Result<Self, ServerError> {
        let runtimes = &server.pool.runtimes;
        let wanted = (runtimes.len() / 2).clamp(2, MAX_QUIC_SHARDS);
        let fewer = |shards: usize| {
            crate::log!("[gm:memory] the buffer budget covers {shards} of {wanted} QUIC endpoints");
        };
        if cfg!(target_os = "linux") && runtimes.len() > 1 {
            if let Some((endpoints, _router)) = server.quic_shards(tls.clone(), address, &runtimes[..wanted])? {
                if endpoints.len() < wanted {
                    fewer(endpoints.len());
                }
                return Ok(Self::Shards {
                    shards: runtimes.iter().cloned().zip(endpoints).collect(),
                    #[cfg(all(test, target_os = "linux"))]
                    router: _router,
                });
            }
            fewer(1);
        }
        Ok(Self::Endpoint(server.quic_endpoint(tls, address)?))
    }

    pub(crate) fn local_addr(&self) -> std::io::Result<SocketAddr> {
        match self {
            Self::Endpoint(quic) => quic.local_addr(),
            Self::Shards { shards, .. } => shards[0].1.local_addr(),
        }
    }

    /// Serves until `stopped`, each shard on its runtime.
    pub(crate) fn serve(self, server: &Arc<HttpServer>, stopped: &watch::Receiver<bool>) -> Vec<Service> {
        let serve = |quic| server.clone().serve_quic(quic, cancelled(stopped.clone()));
        match self {
            Self::Endpoint(quic) => vec![Box::pin(serve(quic))],
            Self::Shards { shards, .. } => shards
                .into_iter()
                .map(|(runtime, quic)| -> Service {
                    let serving = runtime.spawn(serve(quic));
                    Box::pin(async move { serving.await? })
                })
                .collect(),
        }
    }
}

async fn bind(address: &str) -> std::io::Result<TcpListener> {
    let Some(port) = address.strip_prefix(':') else {
        return TcpListener::bind(address).await;
    };
    let port: u16 = port
        .parse()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid listener port"))?;
    // Set dual-stack explicitly rather than inheriting the host's IPV6_V6ONLY
    // default. Socket creation/option failure means IPv6 is unavailable; a bind
    // conflict must still fail startup rather than quietly serving only IPv4.
    let socket = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, None).and_then(|socket| {
        socket.set_only_v6(false)?;
        Ok(socket)
    });
    let Ok(socket) = socket else {
        return TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await;
    };
    #[cfg(unix)]
    socket.set_reuse_address(true)?;
    let address = std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port));
    match socket.bind(&address.into()) {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
            ) =>
        {
            TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await
        }
        Err(error) => Err(error),
        Ok(()) => {
            socket.listen(1024)?;
            socket.set_nonblocking(true)?;
            TcpListener::from_std(socket.into())
        }
    }
}

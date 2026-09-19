//! Process-level ownership of listeners, certificate renewal, and shutdown.

use crate::{
    config::{Config, ConfigError, NativeKind},
    http_server::HttpServer,
    tls::Certificates,
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{future::Future, pin::Pin, sync::Arc, time::SystemTime};
use tokio::{net::TcpListener, sync::watch};

type Service = Pin<Box<dyn Future<Output = Result<(), ConfigError>> + Send>>;

/// Bind every configured socket before serving any request. A bind failure
/// drops all previously opened sockets. Every running service is owned here;
/// normal shutdown waits for their connection tasks and certificate reads.
pub async fn run(config: Config, shutdown: impl Future<Output = ()>) -> Result<(), ConfigError> {
    config.validate()?;
    let config = Arc::new(config);
    let server = Arc::new(HttpServer::new(config.clone())?);
    let tls = if [NativeKind::H1Tls, NativeKind::H2, NativeKind::H3]
        .into_iter()
        .any(|kind| !config.listener(kind).address.is_empty())
    {
        Some(Certificates::load(&config, SystemTime::now())?)
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
        let identity = match kind {
            NativeKind::H1 => None,
            NativeKind::H1Tls | NativeKind::H2 | NativeKind::H3 => Some(
                tls.as_ref()
                    .expect("TLS identity loaded")
                    .config(vec![match kind {
                        NativeKind::H1Tls | NativeKind::H3 => b"http/1.1".to_vec(),
                        _ => b"h2".to_vec(),
                    }])?,
            ),
        };
        if kind == NativeKind::H3 {
            // The bootstrap companion and QUIC endpoint share the actual port,
            // including when the caller asks the OS to allocate one with :0.
            let identity = tls
                .as_ref()
                .expect("TLS identity loaded")
                .config(vec![b"h3".to_vec()])?;
            quic = Some(quinn::Endpoint::server(
                server.quic_config(identity)?,
                listener.local_addr()?,
            )?);
        }
        listeners.push((kind, listener, identity));
    }
    if listeners.is_empty() {
        return Err("at least one listener must be enabled".into());
    }

    let (stop, stopped) = watch::channel(false);
    let mut services = FuturesUnordered::<Service>::new();
    for (kind, listener, identity) in listeners {
        eprintln!(
            "graphite-meter Rust: {} on {}",
            kind.name(),
            listener.local_addr()?
        );
        let server = server.clone();
        let stopped = stopped.clone();
        services.push(Box::pin(async move {
            match kind {
                NativeKind::H1 => server.serve_http1(listener, cancelled(stopped)).await,
                NativeKind::H1Tls => {
                    server
                        .serve_https1(
                            listener,
                            identity.expect("TLS identity"),
                            cancelled(stopped),
                        )
                        .await
                }
                NativeKind::H2 => {
                    server
                        .serve_http2(
                            listener,
                            identity.expect("TLS identity"),
                            cancelled(stopped),
                        )
                        .await
                }
                NativeKind::H3 => {
                    server
                        .serve_https_bootstrap(
                            listener,
                            identity.expect("TLS identity"),
                            cancelled(stopped),
                        )
                        .await
                }
            }
        }));
    }
    if let Some(endpoint) = quic {
        eprintln!(
            "graphite-meter Rust: http3 UDP on {}",
            endpoint.local_addr()?
        );
        let server = server.clone();
        let stopped = stopped.clone();
        services.push(Box::pin(async move {
            server.serve_quic(endpoint, cancelled(stopped)).await
        }));
    }
    if let Some(tls) = tls {
        services.push(Box::pin(async move {
            tls.watch(cancelled(stopped), |result| {
                if let Err(error) = result {
                    eprintln!("TLS certificate renewal rejected: {error}");
                }
            })
            .await
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

async fn bind(address: &str) -> std::io::Result<TcpListener> {
    let Some(port) = address.strip_prefix(':') else {
        return TcpListener::bind(address).await;
    };
    let port: u16 = port.parse().map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid listener port")
    })?;
    // Set dual-stack explicitly rather than inheriting the host's IPV6_V6ONLY
    // default. Socket creation/option failure means IPv6 is unavailable; a bind
    // conflict must still fail startup rather than quietly serving only IPv4.
    let socket = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, None).and_then(
        |socket| {
            socket.set_only_v6(false)?;
            Ok(socket)
        },
    );
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

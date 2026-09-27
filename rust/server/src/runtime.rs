//! Process-level ownership of listeners, certificate renewal, and shutdown.

use crate::{
    config::{AuthMode, Config, ConfigError, NativeKind},
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
    server.initialize_auth().await?;
    let tls = if [NativeKind::H1Tls, NativeKind::H2, NativeKind::H3]
        .into_iter()
        .any(|kind| !config.listener(kind).address.is_empty())
    {
        let server = server.clone();
        Some(
            Certificates::load(&config, SystemTime::now(), move |bytes| server.cover_handshake(bytes))
                .map_err(|error| format!("TLS certificate {} or key {}: {error}", config.tls_cert, config.tls_key))?,
        )
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
            NativeKind::H1Tls | NativeKind::H2 | NativeKind::H3 => {
                Some(tls.as_ref().expect("TLS identity loaded").config(vec![match kind {
                    NativeKind::H1Tls | NativeKind::H3 => b"http/1.1".to_vec(),
                    _ => b"h2".to_vec(),
                }])?)
            }
        };
        if kind == NativeKind::H3 {
            // The bootstrap companion and QUIC endpoint share the actual port,
            // including when the caller asks the OS to allocate one with :0.
            let identity = tls
                .as_ref()
                .expect("TLS identity loaded")
                .config(vec![b"h3".to_vec()])?;
            quic = Some(server.quic_endpoint(identity, listener.local_addr()?)?);
        }
        listeners.push((kind, listener, identity));
    }
    if listeners.is_empty() {
        return Err("at least one listener must be enabled".into());
    }

    let (stop, stopped) = watch::channel(false);
    let mut services = FuturesUnordered::<Service>::new();
    for (kind, listener, identity) in listeners {
        let role = match kind {
            NativeKind::H1 if config.auth.mode != AuthMode::Off => {
                "HTTP/1.1 clear: trusted proxy upstream only; direct requests are refused, GET / redirects to HTTPS"
            }
            NativeKind::H1 => "HTTP/1.1 clear: UI, discovery, probe, transfers, WebSockets",
            NativeKind::H1Tls => "HTTPS/WSS HTTP/1.1: UI, discovery, probe, transfers, WebSockets",
            NativeKind::H2 => "HTTPS HTTP/2: measurement probe, transfers, progress only",
            NativeKind::H3 => "HTTPS HTTP/1.1 companion: HTTP/3 bootstrap probe, upload and ticket control",
        };
        crate::log!(
            "graphite-meter {} listening on {}/tcp ({role})",
            crate::config::ENGINE_VERSION,
            listener.local_addr()?,
        );
        let server = server.clone();
        let stopped = stopped.clone();
        services.push(Box::pin(async move {
            match kind {
                NativeKind::H1 => server.serve_http1(listener, cancelled(stopped)).await,
                NativeKind::H1Tls => {
                    server
                        .serve_https1(listener, identity.expect("TLS identity"), cancelled(stopped))
                        .await
                }
                NativeKind::H2 => {
                    server
                        .serve_http2(listener, identity.expect("TLS identity"), cancelled(stopped))
                        .await
                }
                NativeKind::H3 => {
                    server
                        .serve_https_bootstrap(listener, identity.expect("TLS identity"), cancelled(stopped))
                        .await
                }
            }
        }));
    }
    if let Some(quic) = quic {
        crate::log!(
            "graphite-meter {} listening on {}/udp (HTTP/3: probe, transfers, progress, WebTransport)",
            crate::config::ENGINE_VERSION,
            quic.local_addr()?,
        );
        let server = server.clone();
        let stopped = stopped.clone();
        services.push(Box::pin(
            async move { server.serve_quic(quic, cancelled(stopped)).await },
        ));
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
            let mut transfer_tick = tokio::time::interval(std::time::Duration::from_secs(1));
            let mut admission_tick = tokio::time::interval(std::time::Duration::from_secs(30));
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

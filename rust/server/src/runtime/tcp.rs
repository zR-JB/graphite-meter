//! TCP listeners: each enabled listener's socket with its TLS acceptor, bound before any serves.

use super::{Listening, Socket};
use crate::{
    app::Endpoint,
    config::{Config, ListenerKind, path_error},
    transport::tls::Certificates,
};
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};
use tokio::net::TcpListener;

/// Binds every enabled listener's TCP socket, HTTP/3's for its companion, and names the ports they bound in `config`.
pub(super) async fn bind_all(
    config: &mut Config,
    certificates: Option<&Arc<Certificates>>,
) -> Result<Vec<Listening>, String> {
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

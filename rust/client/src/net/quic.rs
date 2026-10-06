//! QUIC dialing: 3 s per address to answer, then 5 s for the handshake; connection and driver stay on its runtime.
use super::fault::Fault;
use graphite_meter_http3::{Code, client};
use graphite_meter_net::{ConnectError, Verify, bind_udp, client_config, quic::client_transport, resolve};
use graphite_meter_proto::{
    discovery::Protocol,
    origin::{Host, Origin},
};
use noq::crypto::rustls::QuicClientConfig;
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    runtime::Handle,
    task::{AbortHandle, JoinHandle},
    time::timeout,
};

/// How long an address may stay silent: lost handshake packets on a lossy path take seconds.
const SILENT: Duration = Duration::from_secs(3);
/// How long an answering address may take to finish the handshake.
const ANSWERING: Duration = Duration::from_secs(5);

/// A QUIC connection with its endpoint and HTTP/3 driver; dropping it closes them.
pub struct Quic {
    connection: noq::Connection,
    _endpoint: noq::Endpoint,
    driver: AbortHandle,
    /// The runtime that runs them.
    pub home: Handle,
}

impl Quic {
    pub fn closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }
}

impl Drop for Quic {
    fn drop(&mut self) {
        self.connection.close(Code::H3_NO_ERROR.into(), b"");
        self.driver.abort();
    }
}

/// Dials `origin` on `runtime`, which then runs the connection, endpoint and driver; dropping the dial abandons it.
pub(super) async fn dial(origin: &Origin, verify: Verify, runtime: &Handle) -> Result<Dialed, Fault> {
    let origin = origin.clone();
    let mut dialing = Dialing(runtime.spawn(async move { connect(&origin, verify).await }));
    (&mut dialing.0).await.map_err(|error| Fault::Lost(error.to_string()))?
}

/// A connection and its request sender.
type Dialed = (Arc<Quic>, client::SendRequest);

/// A dial running on another runtime, aborted once nothing awaits it.
struct Dialing(JoinHandle<Result<Dialed, Fault>>);

impl Drop for Dialing {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Each address in turn, IPv4 first; the last address's fault if none connects.
async fn connect(origin: &Origin, verify: Verify) -> Result<Dialed, Fault> {
    let crypto =
        QuicClientConfig::try_from(client_config(verify, Some(Protocol::Http3)).await).map_err(io::Error::other);
    let mut config = noq::ClientConfig::new(Arc::new(crypto.map_err(ConnectError::Io)?));
    config.transport_config(Arc::new(client_transport()));
    let resolved = resolve(&origin.host, origin.port).await;
    let mut addresses = resolved.map_err(ConnectError::Unreachable)?;
    addresses.sort_by_key(|address| !address.ip().to_canonical().is_ipv4());
    let name = match &origin.host {
        Host::Name(name) => name.clone(),
        Host::Ip(ip) => ip.to_string(),
    };
    let mut last = ConnectError::Unreachable(io::Error::new(io::ErrorKind::NotFound, "no address resolved")).into();
    for address in addresses {
        match attempt(&config, address, &name).await {
            Ok(dialed) => return Ok(dialed),
            Err(fault) => last = fault,
        }
    }
    Err(last)
}

async fn attempt(config: &noq::ClientConfig, address: SocketAddr, name: &str) -> Result<Dialed, Fault> {
    let local = match address {
        SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    };
    let (socket, _) = bind_udp(local, 1).map_err(ConnectError::Io)?;
    let runtime = noq::default_runtime().ok_or_else(|| io::Error::other("no runtime for QUIC"));
    let endpoint = noq::Endpoint::new(noq::EndpointConfig::default(), None, socket, runtime.map_err(ConnectError::Io)?);
    let endpoint = endpoint.map_err(ConnectError::Io)?;
    let connecting = endpoint.connect_with(config.clone(), address, name);
    let mut connecting = connecting.map_err(|error| ConnectError::Io(io::Error::other(error)))?;
    let silent = || ConnectError::Unreachable(io::Error::new(io::ErrorKind::TimedOut, "no QUIC answer"));
    let answered = timeout(SILENT, connecting.handshake_data()).await;
    answered.map_err(|_| silent())?.map_err(refused)?;
    let finished = timeout(ANSWERING, connecting).await;
    let connection = finished
        .map_err(|_| Fault::TimedOut("QUIC handshake"))?
        .map_err(refused)?;
    let (mut driver, requests) = client::new(connection.clone());
    let driver = tokio::spawn(async move {
        let _ = driver.drive().await;
    });
    let quic = Quic {
        connection,
        _endpoint: endpoint,
        driver: driver.abort_handle(),
        home: Handle::current(),
    };
    Ok((Arc::new(quic), requests))
}

/// A handshake's failure, with the TLS error a certificate check raised.
fn refused(error: noq::ConnectionError) -> Fault {
    let tls = match &error {
        noq::ConnectionError::TransportError(transport) => transport.crypto.as_deref(),
        _ => None,
    };
    match tls.and_then(|tls| tls.downcast_ref::<rustls::Error>()) {
        Some(tls) => ConnectError::Tls(tls.clone()).into(),
        None => ConnectError::Io(io::Error::other(error)).into(),
    }
}

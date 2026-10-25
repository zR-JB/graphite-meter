//! QUIC transport settings for both roles, with the shared receive and send windows, and the client's dial.
use crate::{
    ConnectError, Verify, bind_udp, client_config,
    dial::{TIMEOUT, resolve, share},
};
use graphite_meter_proto::{
    discovery::Protocol,
    lane::{IDLE_BOUND, MAX_LANE_STREAMS},
    origin::Host,
};
use noq::{IdleTimeout, TransportConfig, VarInt, crypto::rustls::QuicClientConfig};
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::time::{Instant, timeout, timeout_at};

const STREAM_RECEIVE_WINDOW: u32 = 32 << 20;
pub const RECEIVE_WINDOW: u32 = 48 << 20;
/// The first connection receive window, from which autotuning starts.
pub const INITIAL_RECEIVE_WINDOW: u32 = 768 << 10;
/// A server connection's receive window until an admitted upload reads: one maximal HTTP/3 frame.
pub const RECEIVE_WINDOW_FLOOR: u32 = 64 << 10;
/// The send window a server connection starts at and tunes back to.
pub const MIN_SEND_WINDOW: u64 = 2 << 20;
/// The client's send window and the ceiling of the server's tuning.
pub const MAX_SEND_WINDOW: u64 = 16 << 20;

const CLIENT_UNI_STREAMS: u32 = 36;
/// The client's three HTTP/3 streams, a WebTransport lane's streams and four streams of credit headroom.
const SERVER_UNI_STREAMS: u32 = 3 + MAX_LANE_STREAMS as u32 + 4;
const CLIENT_DATAGRAM_BUFFER: usize = 256 << 10;
const SERVER_DATAGRAM_BUFFER: usize = 64 << 10;
const CLIENT_IDLE_MILLIS: u32 = 60_000;

/// A client connection's transport: no streams opened by the server but unidirectional ones.
pub fn client_transport() -> TransportConfig {
    let mut transport = TransportConfig::default();
    transport.max_concurrent_bidi_streams(0_u32.into());
    transport.max_concurrent_uni_streams(CLIENT_UNI_STREAMS.into());
    transport.stream_receive_window(STREAM_RECEIVE_WINDOW.into());
    transport.receive_window(RECEIVE_WINDOW.into());
    transport.initial_receive_window(Some(INITIAL_RECEIVE_WINDOW.into()));
    transport.send_window(MAX_SEND_WINDOW);
    transport.datagram_receive_buffer_size(Some(CLIENT_DATAGRAM_BUFFER));
    transport.max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(
        CLIENT_IDLE_MILLIS,
    ))));
    transport
}

/// A server connection's transport, accepting `requests` request streams and idling out after the lane bound.
pub fn server_transport(requests: u32) -> TransportConfig {
    let mut transport = TransportConfig::default();
    transport.max_concurrent_bidi_streams(requests.into());
    transport.max_concurrent_uni_streams(SERVER_UNI_STREAMS.into());
    transport.stream_receive_window(STREAM_RECEIVE_WINDOW.into());
    transport.receive_window(RECEIVE_WINDOW_FLOOR.into());
    transport.initial_receive_window(Some(INITIAL_RECEIVE_WINDOW.into()));
    transport.send_window(MIN_SEND_WINDOW);
    transport.datagram_receive_buffer_size(Some(SERVER_DATAGRAM_BUFFER));
    transport.datagram_send_buffer_size(SERVER_DATAGRAM_BUFFER);
    const IDLE_MILLIS: u32 = {
        let millis = IDLE_BOUND.as_millis();
        assert!(
            millis <= u32::MAX as u128,
            "the lane bound fits QUIC's idle timeout"
        );
        millis as u32
    };
    transport.max_idle_timeout(Some(IdleTimeout::from(VarInt::from_u32(IDLE_MILLIS))));
    transport
}

/// How long an address may stay silent: lost handshake packets on a lossy path take seconds.
const SILENT: Duration = Duration::from_secs(3);
/// How long an answering address may take to finish the handshake.
const ANSWERING: Duration = Duration::from_secs(5);

/// A client connection to `host` on its own endpoint: each address in turn, IPv4 first, each with its share of the dial timeout.
pub async fn dial(
    host: &Host,
    port: u16,
    verify: &Verify,
) -> Result<(noq::Endpoint, noq::Connection), ConnectError> {
    let crypto = QuicClientConfig::try_from(client_config(verify, Some(Protocol::Http3)).await);
    let mut config = noq::ClientConfig::new(Arc::new(
        crypto.map_err(|error| ConnectError::Io(io::Error::other(error)))?,
    ));
    config.transport_config(Arc::new(client_transport()));
    let name = match host {
        Host::Name(name) => name.clone(),
        Host::Ip(ip) => ip.to_string(),
    };
    let overran = || ConnectError::Unreachable(io::ErrorKind::TimedOut.into());
    let deadline = Instant::now() + TIMEOUT;
    let dialing = async {
        let mut addresses = resolve(host, port)
            .await
            .map_err(ConnectError::Unreachable)?;
        addresses.sort_by_key(|address| !address.ip().to_canonical().is_ipv4());
        let mut last = ConnectError::Unreachable(io::Error::new(
            io::ErrorKind::NotFound,
            "no address resolved",
        ));
        for (index, &address) in addresses.iter().enumerate() {
            let share = share(
                deadline.saturating_duration_since(Instant::now()),
                addresses.len() - index,
            );
            match timeout(share, attempt(&config, address, &name)).await {
                Ok(Ok(dialed)) => return Ok(dialed),
                Ok(Err(error)) => last = error,
                Err(_) => last = overran(),
            }
        }
        Err(last)
    };
    timeout_at(deadline, dialing)
        .await
        .unwrap_or_else(|_| Err(overran()))
}

async fn attempt(
    config: &noq::ClientConfig,
    address: SocketAddr,
    name: &str,
) -> Result<(noq::Endpoint, noq::Connection), ConnectError> {
    let local = match address {
        SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    };
    let (socket, _) = bind_udp(local, 1).map_err(ConnectError::Io)?;
    let runtime = noq::default_runtime()
        .ok_or_else(|| ConnectError::Io(io::Error::other("no runtime for QUIC")))?;
    let endpoint = noq::Endpoint::new(noq::EndpointConfig::default(), None, socket, runtime)
        .map_err(ConnectError::Io)?;
    let mut connecting = endpoint
        .connect_with(config.clone(), address, name)
        .map_err(|error| ConnectError::Io(io::Error::other(error)))?;
    let overran = |what| ConnectError::Unreachable(io::Error::new(io::ErrorKind::TimedOut, what));
    let answered = timeout(SILENT, connecting.handshake_data()).await;
    answered
        .map_err(|_| overran("no QUIC answer"))?
        .map_err(refused)?;
    let finished = timeout(ANSWERING, connecting).await;
    let connection = finished
        .map_err(|_| overran("QUIC handshake timed out"))?
        .map_err(refused)?;
    Ok((endpoint, connection))
}

/// A handshake's failure, with the TLS error a certificate check raised.
fn refused(error: noq::ConnectionError) -> ConnectError {
    let tls = match &error {
        noq::ConnectionError::TransportError(transport) => transport.crypto.as_deref(),
        _ => None,
    };
    match tls.and_then(|tls| tls.downcast_ref::<rustls::Error>()) {
        Some(tls) => ConnectError::Tls(tls.clone()),
        None => ConnectError::Io(io::Error::other(error)),
    }
}

use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
};

/// quic-go's desired UDP buffer size.
const BUFFER_BYTES: usize = 7 * 1024 * 1024;
/// What each of several sockets keeps of it: a single fast connection's headroom.
const SHARED_BUFFER_FLOOR: usize = 2 * 1024 * 1024;
/// quic-go warns once per process.
static WARNED: AtomicBool = AtomicBool::new(false);

/// Sizes the buffers as quic-go does. Like quic-go, the first socket whose
/// buffers stay short returns its warning, unless
/// `QUIC_GO_DISABLE_RECEIVE_BUFFER_WARNING` is true.
pub fn udp_socket(address: SocketAddr) -> io::Result<(UdpSocket, Option<String>)> {
    udp_socket_with(address, 1)
}

/// [`udp_socket`] for one of `sockets` that share the address with `SO_REUSEPORT`
/// and split quic-go's buffer size, down to a floor. Only Linux balances unicast
/// datagrams among them, so other targets refuse several.
pub fn udp_socket_with(address: SocketAddr, sockets: usize) -> io::Result<(UdpSocket, Option<String>)> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    if address.is_ipv6() {
        let _ = socket.set_only_v6(false);
    }
    let bytes = buffer_bytes(sockets);
    let receive = grow(&socket, Buffer::Receive, bytes);
    let send = grow(&socket, Buffer::Send, bytes);
    // Both buffers grow; the receive buffer's shortfall is reported first.
    let shortfall = receive.and(send).err();
    let silenced = quiet(std::env::var("QUIC_GO_DISABLE_RECEIVE_BUFFER_WARNING").ok().as_deref());
    let reported = shortfall.filter(|_| !WARNED.swap(true, Ordering::Relaxed) && !silenced);
    let link = "See https://github.com/quic-go/quic-go/wiki/UDP-Buffer-Sizes for details.";
    let warning = reported.map(|shortfall| format!("{shortfall}. {link}"));
    if sockets > 1 {
        #[cfg(target_os = "linux")]
        socket.set_reuse_port(true)?;
        #[cfg(not(target_os = "linux"))]
        return Err(io::Error::new(io::ErrorKind::Unsupported, "SO_REUSEPORT balances UDP only on Linux"));
    }
    socket.bind(&address.into())?;
    Ok((socket.into(), warning))
}

fn buffer_bytes(sockets: usize) -> usize {
    (BUFFER_BYTES / sockets.max(1)).max(SHARED_BUFFER_FLOOR)
}

#[derive(Clone, Copy)]
enum Buffer {
    Receive,
    Send,
}

/// quic-go's setReceiveBuffer and setSendBuffer, including their messages, for `bytes`.
fn grow(socket: &socket2::Socket, buffer: Buffer, bytes: usize) -> Result<(), String> {
    let name = match buffer {
        Buffer::Receive => "receive",
        Buffer::Send => "send",
    };
    let size = || {
        match buffer {
            Buffer::Receive => socket.recv_buffer_size(),
            Buffer::Send => socket.send_buffer_size(),
        }
        .map_err(|error| format!("failed to determine {name} buffer size: {error}"))
    };
    let before = size()?;
    if before >= bytes {
        return Ok(());
    }
    let _ = match buffer {
        Buffer::Receive => socket.set_recv_buffer_size(bytes),
        Buffer::Send => socket.set_send_buffer_size(bytes),
    };
    #[cfg(target_os = "linux")]
    if size()? < bytes {
        // Privileged processes may exceed the sysctl maximum.
        let _ = match buffer {
            Buffer::Receive => rustix::net::sockopt::set_socket_recv_buffer_size_force(socket, bytes),
            Buffer::Send => rustix::net::sockopt::set_socket_send_buffer_size_force(socket, bytes),
        };
    }
    shortfall(name, before, size()?, bytes).map_or(Ok(()), Err)
}

/// quic-go's message for a buffer it could not grow to the desired size.
fn shortfall(name: &str, before: usize, after: usize, bytes: usize) -> Option<String> {
    let (wanted, got) = (bytes / 1024, after / 1024);
    match after {
        _ if after >= bytes => None,
        _ if after == before => {
            Some(format!("failed to increase {name} buffer size (wanted: {wanted} kiB, got {got} kiB)"))
        }
        _ => Some(format!(
            "failed to sufficiently increase {name} buffer size (was: {} kiB, wanted: {wanted} kiB, got: {got} kiB)",
            before / 1024
        )),
    }
}

/// Go's strconv.ParseBool; anything else leaves the warning on.
fn quiet(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "t" | "T" | "TRUE" | "true" | "True"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortfalls_read_like_quic_go() {
        assert_eq!(
            shortfall("receive", 212_992, 425_984, BUFFER_BYTES).as_deref(),
            Some("failed to sufficiently increase receive buffer size (was: 208 kiB, wanted: 7168 kiB, got: 416 kiB)")
        );
        assert_eq!(
            shortfall("send", 212_992, 212_992, buffer_bytes(2)).as_deref(),
            Some("failed to increase send buffer size (wanted: 3584 kiB, got 208 kiB)")
        );
        assert_eq!(shortfall("receive", 212_992, BUFFER_BYTES, BUFFER_BYTES), None);
        assert_eq!([1, 2, 16].map(buffer_bytes), [7 << 20, 7 << 19, 2 << 20], "a floor past a few sockets");
    }

    #[test]
    fn only_go_true_values_silence_the_warning() {
        for value in ["1", "t", "T", "TRUE", "true", "True"] {
            assert!(quiet(Some(value)), "{value}");
        }
        for value in [None, Some(""), Some("yes"), Some("0"), Some("false"), Some(" true")] {
            assert!(!quiet(value), "{value:?}");
        }
    }

    #[test]
    fn a_socket_either_gets_its_buffers_or_a_warning_once() {
        let (first, warning) = udp_socket("127.0.0.1:0".parse().unwrap()).unwrap();
        let socket = socket2::SockRef::from(&first);
        let short =
            socket.recv_buffer_size().unwrap() < BUFFER_BYTES || socket.send_buffer_size().unwrap() < BUFFER_BYTES;
        if let Some(warning) = &warning {
            assert!(warning.ends_with(". See https://github.com/quic-go/quic-go/wiki/UDP-Buffer-Sizes for details."));
        }
        assert!(!short || warning.is_some() || WARNED.load(Ordering::Relaxed));
        assert!(udp_socket("127.0.0.1:0".parse().unwrap()).unwrap().1.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reuse_port_sockets_share_an_address_that_others_cannot_take() {
        let (first, _) = udp_socket_with("127.0.0.1:0".parse().unwrap(), 2).unwrap();
        let address = first.local_addr().unwrap();
        let (second, _) = udp_socket_with(address, 2).unwrap();
        assert_eq!(second.local_addr().unwrap(), address);
        assert!(udp_socket(address).is_err());
    }
}

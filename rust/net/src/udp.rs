//! UDP sockets for QUIC, with quic-go's buffer sizes and warning.
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
};

/// quic-go's desired UDP buffer size.
const BUFFER_BYTES: usize = 7 << 20;
/// The least each of several sockets on one address keeps: a single fast connection's headroom.
const SHARED_BUFFER_FLOOR: usize = 2 << 20;
/// Whether a socket of this process has returned the warning.
static WARNED: AtomicBool = AtomicBool::new(false);

/// Binds one of `sockets` sharing `address` through `SO_REUSEPORT` (Linux only), returning quic-go's warning for
/// the first socket whose buffers stay short unless `QUIC_GO_DISABLE_RECEIVE_BUFFER_WARNING` is true.
pub fn bind_udp(address: SocketAddr, sockets: usize) -> io::Result<(UdpSocket, Option<String>)> {
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
    let silenced = quiet(std::env::var("QUIC_GO_DISABLE_RECEIVE_BUFFER_WARNING").ok().as_deref());
    let shortfall = receive.and(send).err();
    let reported = shortfall.filter(|_| !silenced && !WARNED.swap(true, Ordering::Relaxed));
    let warning = reported.map(|shortfall| {
        format!("{shortfall}. See https://github.com/quic-go/quic-go/wiki/UDP-Buffer-Sizes for details.")
    });
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

/// quic-go's setReceiveBuffer and setSendBuffer for `bytes`, forcing past the system maximum where allowed.
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
        let _ = match buffer {
            Buffer::Receive => rustix::net::sockopt::set_socket_recv_buffer_size_force(socket, bytes),
            Buffer::Send => rustix::net::sockopt::set_socket_send_buffer_size_force(socket, bytes),
        };
    }
    shortfall(name, before, size()?, bytes).map_or(Ok(()), Err)
}

/// quic-go's message for a buffer that stayed below `bytes`.
fn shortfall(name: &str, before: usize, after: usize, bytes: usize) -> Option<String> {
    let (was, wanted, got) = (before / 1024, bytes / 1024, after / 1024);
    match after {
        _ if after >= bytes => None,
        _ if after == before => {
            Some(format!("failed to increase {name} buffer size (wanted: {wanted} kiB, got {got} kiB)"))
        }
        _ => Some(format!(
            "failed to sufficiently increase {name} buffer size (was: {was} kiB, wanted: {wanted} kiB, got: {got} kiB)"
        )),
    }
}

/// Go's strconv.ParseBool true values.
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
}

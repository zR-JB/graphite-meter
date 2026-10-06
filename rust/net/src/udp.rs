//! UDP sockets for QUIC, with large buffers and one warning when the host caps them.
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
};

/// The UDP buffer each QUIC socket asks for.
const BUFFER_BYTES: usize = 7 << 20;
/// The least each of several sockets on one address keeps: a single fast connection's headroom.
const SHARED_BUFFER_FLOOR: usize = 2 << 20;
/// Whether a socket of this process has returned the warning.
static WARNED: AtomicBool = AtomicBool::new(false);

/// Binds one of `sockets` sharing `address` through `SO_REUSEPORT` (Linux only), returning a warning for the first
/// socket of this process whose buffers stay short.
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
    let shortfall = receive.and(send).err();
    let reported = shortfall.filter(|_| !WARNED.swap(true, Ordering::Relaxed));
    let warning = reported.map(|shortfall| {
        format!(
            "[gm:udp] {shortfall}, so QUIC above about 1 Gbit/s may drop packets; raise net.core.rmem_max and \
             net.core.wmem_max on the host (docs/DEPLOYMENT.md, UDP buffers)"
        )
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
    shortfall(name, size()?, bytes).map_or(Ok(()), Err)
}

/// The shortfall of a buffer that stayed below `bytes`.
fn shortfall(name: &str, after: usize, bytes: usize) -> Option<String> {
    (after < bytes).then(|| format!("the UDP {name} buffer is {} KiB of the {} KiB wanted", after / 1024, bytes / 1024))
}

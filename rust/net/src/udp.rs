//! UDP sockets for QUIC, with large buffers.
use std::{
    io,
    net::{SocketAddr, UdpSocket},
};

const BUFFER_BYTES: usize = 7 << 20;
/// The least each of several sockets on one address keeps: a single fast connection's headroom.
const SHARED_BUFFER_FLOOR: usize = 2 << 20;

/// Binds the first of `sockets` on `address`, refused while any socket holds it; `join_udp` binds the rest beside it.
pub fn bind_udp(address: SocketAddr, sockets: usize) -> io::Result<(UdpSocket, Option<String>)> {
    let (socket, shortfall) = udp(address, sockets)?;
    socket.bind(&address.into())?;
    if sockets > 1 {
        share(&socket)?;
    }
    Ok((socket.into(), shortfall))
}

pub fn join_udp(address: SocketAddr, sockets: usize) -> io::Result<(UdpSocket, Option<String>)> {
    let (socket, shortfall) = udp(address, sockets)?;
    share(&socket)?;
    socket.bind(&address.into())?;
    Ok((socket.into(), shortfall))
}

fn udp(address: SocketAddr, sockets: usize) -> io::Result<(socket2::Socket, Option<String>)> {
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
    Ok((socket, receive.and(send).err()))
}

fn share(socket: &socket2::Socket) -> io::Result<()> {
    cfg_select! {
        target_os = "linux" => socket.set_reuse_port(true),
        _ => {
            let _ = socket;
            Err(io::Error::new(io::ErrorKind::Unsupported, "SO_REUSEPORT balances UDP only on Linux"))
        }
    }
}

fn buffer_bytes(sockets: usize) -> usize {
    (BUFFER_BYTES / sockets.max(1)).max(SHARED_BUFFER_FLOOR)
}

#[derive(Clone, Copy)]
enum Buffer {
    Receive,
    Send,
}

/// Grows a buffer to `bytes`, forcing past the system maximum where allowed.
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
            Buffer::Receive => {
                rustix::net::sockopt::set_socket_recv_buffer_size_force(socket, bytes)
            }
            Buffer::Send => rustix::net::sockopt::set_socket_send_buffer_size_force(socket, bytes),
        };
    }
    let after = size()?;
    match after < bytes {
        true => Err(format!(
            "{name} buffer {} KiB of {} KiB wanted",
            after / 1024,
            bytes / 1024
        )),
        false => Ok(()),
    }
}

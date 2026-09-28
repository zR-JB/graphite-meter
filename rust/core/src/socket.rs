use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
};

/// quic-go's desired UDP buffer size.
const BUFFER_BYTES: usize = 7 * 1024 * 1024;
/// quic-go warns once per process.
static WARNED: AtomicBool = AtomicBool::new(false);

/// Sizes the buffers as quic-go does. Like quic-go, the first socket whose
/// buffers stay short returns its warning, unless
/// `QUIC_GO_DISABLE_RECEIVE_BUFFER_WARNING` is true.
pub fn udp_socket(address: SocketAddr) -> io::Result<(UdpSocket, Option<String>)> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    if address.is_ipv6() {
        let _ = socket.set_only_v6(false);
    }
    let receive = grow(&socket, Buffer::Receive);
    let send = grow(&socket, Buffer::Send);
    let warning = receive
        .err()
        .or(send.err())
        .filter(|_| {
            !WARNED.swap(true, Ordering::Relaxed)
                && !quiet(std::env::var("QUIC_GO_DISABLE_RECEIVE_BUFFER_WARNING").ok().as_deref())
        })
        .map(|shortfall| {
            format!("{shortfall}. See https://github.com/quic-go/quic-go/wiki/UDP-Buffer-Sizes for details.")
        });
    socket.bind(&address.into())?;
    Ok((socket.into(), warning))
}

#[derive(Clone, Copy)]
enum Buffer {
    Receive,
    Send,
}

/// quic-go's setReceiveBuffer and setSendBuffer, including their messages.
fn grow(socket: &socket2::Socket, buffer: Buffer) -> Result<(), String> {
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
    if before >= BUFFER_BYTES {
        return Ok(());
    }
    let _ = match buffer {
        Buffer::Receive => socket.set_recv_buffer_size(BUFFER_BYTES),
        Buffer::Send => socket.set_send_buffer_size(BUFFER_BYTES),
    };
    #[cfg(target_os = "linux")]
    if size()? < BUFFER_BYTES {
        // Privileged processes may exceed the sysctl maximum.
        let _ = match buffer {
            Buffer::Receive => rustix::net::sockopt::set_socket_recv_buffer_size_force(socket, BUFFER_BYTES),
            Buffer::Send => rustix::net::sockopt::set_socket_send_buffer_size_force(socket, BUFFER_BYTES),
        };
    }
    shortfall(name, before, size()?).map_or(Ok(()), Err)
}

/// quic-go's message for a buffer it could not grow to the desired size.
fn shortfall(name: &str, before: usize, after: usize) -> Option<String> {
    let (wanted, got) = (BUFFER_BYTES / 1024, after / 1024);
    match after {
        _ if after >= BUFFER_BYTES => None,
        _ if after == before => Some(format!(
            "failed to increase {name} buffer size (wanted: {wanted} kiB, got {got} kiB)"
        )),
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
            shortfall("receive", 212_992, 425_984).as_deref(),
            Some("failed to sufficiently increase receive buffer size (was: 208 kiB, wanted: 7168 kiB, got: 416 kiB)")
        );
        assert_eq!(
            shortfall("send", 212_992, 212_992).as_deref(),
            Some("failed to increase send buffer size (wanted: 7168 kiB, got 208 kiB)")
        );
        assert_eq!(shortfall("receive", 212_992, BUFFER_BYTES), None);
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
}

use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::Once,
};

pub fn udp_socket(address: SocketAddr) -> io::Result<UdpSocket> {
    const BUFFER_BYTES: usize = 7 * 1024 * 1024;
    static REPORT: Once = Once::new();
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    if address.is_ipv6() {
        let _ = socket.set_only_v6(false);
    }
    let _ = socket.set_recv_buffer_size(BUFFER_BYTES);
    let _ = socket.set_send_buffer_size(BUFFER_BYTES);
    let receive = socket.recv_buffer_size().unwrap_or_default();
    let send = socket.send_buffer_size().unwrap_or_default();
    if receive < BUFFER_BYTES || send < BUFFER_BYTES {
        REPORT.call_once(|| {
            eprintln!(
                "UDP buffers below requested {BUFFER_BYTES} bytes: receive={receive}, send={send}"
            )
        });
    }
    socket.bind(&address.into())?;
    Ok(socket.into())
}

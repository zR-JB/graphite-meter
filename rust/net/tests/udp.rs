//! QUIC sockets: quic-go's buffer sizes or its warning, and address sharing on Linux.
use graphite_meter_net::bind_udp;

const LINK: &str = ". See https://github.com/quic-go/quic-go/wiki/UDP-Buffer-Sizes for details.";

#[test]
fn a_socket_with_short_buffers_warns_once_per_process() {
    let mut warnings = Vec::new();
    for _ in 0..3 {
        let (socket, warning) = bind_udp("127.0.0.1:0".parse().unwrap(), 1).unwrap();
        let socket = socket2::SockRef::from(&socket);
        let full = socket.recv_buffer_size().unwrap() >= 7 << 20 && socket.send_buffer_size().unwrap() >= 7 << 20;
        assert!(!full || warning.is_none(), "{warning:?}");
        warnings.extend(warning);
    }
    assert!(warnings.len() <= 1, "{warnings:?}");
    assert!(warnings.iter().all(|warning| warning.ends_with(LINK)), "{warnings:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn sharing_sockets_take_one_address_that_others_cannot() {
    let (first, _) = bind_udp("127.0.0.1:0".parse().unwrap(), 2).unwrap();
    let address = first.local_addr().unwrap();
    let (second, _) = bind_udp(address, 2).unwrap();
    assert_eq!(second.local_addr().unwrap(), address);
    assert!(bind_udp(address, 1).is_err());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn sharing_sockets_are_refused_where_the_kernel_does_not_balance_them() {
    let refused = bind_udp("127.0.0.1:0".parse().unwrap(), 2).unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::Unsupported);
}

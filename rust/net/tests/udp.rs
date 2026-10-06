//! QUIC sockets: address sharing on Linux.
use graphite_meter_net::bind_udp;

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

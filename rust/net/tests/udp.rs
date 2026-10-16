//! QUIC sockets: address sharing on Linux.
#![cfg(target_os = "linux")]
use graphite_meter_net::bind_udp;

#[test]
fn sharing_sockets_take_one_address_that_others_cannot() {
    let (first, _) = bind_udp("127.0.0.1:0".parse().unwrap(), 2).unwrap();
    let address = first.local_addr().unwrap();
    let (second, _) = bind_udp(address, 2).unwrap();
    assert_eq!(second.local_addr().unwrap(), address);
    assert!(bind_udp(address, 1).is_err());
}

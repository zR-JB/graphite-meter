use graphite_meter_core::wire::decode_pong;
use graphite_meter_server::ping;
use std::time::Instant;

#[test]
fn replies_preserve_boundary_ids_and_bound_handling_to_endpoint_time() {
    for id in [0, 1, 1, u32::MAX] {
        let message = format!("PING,{id}");
        let started = Instant::now();
        let reply = ping::reply(message.as_bytes()).expect("valid ping must receive a reply");
        let elapsed = started.elapsed().as_nanos();
        let pong = decode_pong(&reply).expect("reply must match the public wire format");

        assert_eq!(pong.id, id);
        assert!(u128::from(pong.handling_nanos) <= elapsed);
    }
}

#[test]
fn malformed_messages_are_ignored_without_affecting_following_probes() {
    for message in [
        b"PNG,5".as_slice(),
        b"PING,5,0",
        b"PING,4294967296",
        b"PING,\xff",
        b"",
    ] {
        assert!(ping::reply(message).is_none());
    }

    let reply = ping::reply(b"PING,7").expect("valid ping following malformed messages");
    assert_eq!(decode_pong(&reply).unwrap().id, 7);
}

use graphite_meter_proto::bus::{Ping, Pong};

#[test]
fn bus_messages_pass_the_shared_vectors() {
    let vectors = include_str!("../../../api/wire.testvectors.txt");
    for line in vectors
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let [operation, input, expected] = line.split(" | ").collect::<Vec<_>>()[..] else {
            panic!("malformed vector {line:?}");
        };
        let pong = |id: &str, nanos: &str| Pong {
            id: id.parse().unwrap(),
            handling_nanos: nanos.parse().unwrap(),
        };
        let actual = match operation {
            "encode-ping" => Some(Ping { id: input.parse().unwrap() }.encode()),
            "encode-pong" => input.split_once(',').map(|(id, nanos)| pong(id, nanos).encode()),
            "decode-ping" => Ping::decode(input.as_bytes()).map(|ping| ping.id.to_string()),
            "decode-pong" => Pong::decode(input.as_bytes()).map(|pong| format!("{},{}", pong.id, pong.handling_nanos)),
            _ => panic!("unknown operation {operation}"),
        };
        let expected = (expected != "INVALID").then_some(expected);
        assert_eq!(actual.as_deref(), expected, "{line}");
    }
}

#[test]
fn messages_have_no_framing_and_unknown_frames_are_malformed() {
    assert_eq!(Ping { id: 7 }.encode().as_bytes(), b"PING,7");
    for message in ["PING,7\n", "PING,7\r\n", " PING,7", "ping,7", "PING,7\0", "HELLO", "READY", "GOODBYE"] {
        assert_eq!(Ping::decode(message.as_bytes()), None, "{message:?}");
    }
    assert_eq!(Ping::decode(b"PING,\xff"), None);
    assert_eq!(Pong::decode(b"PONG,1,2\n"), None);
}

#[test]
fn probe_ids_count_up_and_wrap_and_replies_echo_them() {
    assert_eq!(Ping { id: 41 }.next(), Ping { id: 42 });
    assert_eq!(Ping { id: u32::MAX }.next(), Ping { id: 0 });
    let ping = Ping::decode(b"PING,00042").unwrap();
    assert_eq!(ping.reply(15).encode(), "PONG,42,15");
}

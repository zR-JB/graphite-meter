use graphite_meter_core::capsule::{
    Capsule, Decoder, Error, MAX_STREAMS, MAX_VARINT, encode, encode_close,
};

fn raw(kind: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = varint(kind);
    bytes.extend(varint(body.len() as u64));
    bytes.extend(body);
    bytes
}
fn varint(value: u64) -> Vec<u8> {
    let mut bytes = value.to_be_bytes();
    bytes[0] |= 0xc0;
    bytes.to_vec()
}

#[test]
fn every_split_and_one_byte_feeds() {
    let expected = vec![
        Capsule::CloseSession {
            code: 0xf1234567,
            message: b"done".to_vec(),
        },
        Capsule::MaxData(MAX_VARINT),
        Capsule::MaxStreamsBidi(MAX_STREAMS),
        Capsule::MaxStreamsUni(64),
        Capsule::DataBlocked(16384),
        Capsule::StreamsBlockedBidi(0),
        Capsule::StreamsBlockedUni(1073741824),
    ];
    let bytes: Vec<_> = expected.iter().flat_map(|c| encode(c).unwrap()).collect();
    for split in 0..=bytes.len() {
        let mut decoder = Decoder::new();
        let mut got = decoder.feed(&bytes[..split]).unwrap();
        got.extend(decoder.feed(&bytes[split..]).unwrap());
        assert_eq!(got, expected, "split {split}");
        decoder.finish().unwrap();
    }
    let mut decoder = Decoder::new();
    let got: Vec<_> = bytes
        .iter()
        .flat_map(|byte| decoder.feed(&[*byte]).unwrap())
        .collect();
    assert_eq!(got, expected);
    decoder.finish().unwrap();
}

#[test]
fn unknown_is_skipped_without_declared_length_allocation() {
    assert!(std::mem::size_of::<Decoder>() <= 1080);
    let mut decoder = Decoder::new();
    let mut header = varint(33);
    header.extend(varint(MAX_VARINT));
    assert!(decoder.feed(&header).unwrap().is_empty());
    for _ in 0..256 {
        assert!(decoder.feed(&[0; 4096]).unwrap().is_empty());
    }
    assert_eq!(decoder.finish(), Err(Error::Truncated));
    let mut decoder = Decoder::new();
    let mut bytes = raw(33, &[1; 10000]);
    bytes.extend(raw(34, &[]));
    bytes.extend(encode(&Capsule::MaxData(7)).unwrap());
    assert_eq!(decoder.feed(&bytes).unwrap(), vec![Capsule::MaxData(7)]);
    decoder.finish().unwrap();
}

#[test]
fn close_retains_raw_first_1024_and_consumes_tail() {
    let mut body = 42u32.to_be_bytes().to_vec();
    body.extend([0xff; 4096]);
    let mut bytes = raw(0x2843, &body);
    bytes.extend(encode(&Capsule::MaxData(1)).unwrap());
    let mut decoder = Decoder::new();
    let got: Vec<_> = bytes
        .chunks(7)
        .flat_map(|chunk| decoder.feed(chunk).unwrap())
        .collect();
    assert_eq!(
        got,
        vec![
            Capsule::CloseSession {
                code: 42,
                message: vec![0xff; 1024]
            },
            Capsule::MaxData(1)
        ]
    );
    decoder.finish().unwrap();
}

#[test]
fn close_outgoing_utf8_boundary() {
    let text = format!("{}€tail", "a".repeat(1023));
    let mut decoder = Decoder::new();
    assert_eq!(
        decoder.feed(&encode_close(1, &text)).unwrap(),
        vec![Capsule::CloseSession {
            code: 1,
            message: vec![b'a'; 1023]
        }]
    );
    assert_eq!(
        encode(&Capsule::CloseSession {
            code: 0,
            message: vec![0xff]
        }),
        Err(Error::InvalidUtf8)
    );
}

#[test]
fn nonminimal_headers_and_numeric_payloads_are_accepted() {
    let mut decoder = Decoder::new();
    assert_eq!(
        decoder.feed(&raw(0x190b4d3d, &varint(1))).unwrap(),
        vec![Capsule::MaxData(1)]
    );
    decoder.finish().unwrap();
}

#[test]
fn malformed_and_truncated_capsules() {
    for bytes in [
        raw(0x2843, &[0; 3]),
        raw(0x190b4d3d, &[]),
        raw(0x190b4d3d, &[0; 9]),
        raw(0x190b4d3d, &[0, 0]),
        raw(0x190b4d3d, &[0xc0]),
    ] {
        let mut decoder = Decoder::new();
        assert_eq!(decoder.feed(&bytes), Err(Error::InvalidPayload));
        assert_eq!(decoder.feed(&[]), Err(Error::Failed));
    }
    for kind in [0x190b4d3e, 0x190b4d42] {
        assert_eq!(Decoder::new().feed(&raw(kind, &[])), Err(Error::Http2Only));
    }
    let bytes = raw(0x2843, &[0; 10]);
    for end in 1..bytes.len() {
        let mut decoder = Decoder::new();
        decoder.feed(&bytes[..end]).unwrap();
        assert_eq!(decoder.finish(), Err(Error::Truncated), "end {end}");
    }
    Decoder::new().finish().unwrap();
}

#[test]
fn numeric_bounds() {
    for kind in [0x190b4d3f, 0x190b4d40, 0x190b4d43, 0x190b4d44] {
        assert!(
            Decoder::new()
                .feed(&raw(kind, &varint(MAX_STREAMS)))
                .is_ok()
        );
        assert_eq!(
            Decoder::new().feed(&raw(kind, &varint(MAX_STREAMS + 1))),
            Err(Error::ValueOutOfRange)
        );
    }
    for capsule in [
        Capsule::MaxData(MAX_VARINT + 1),
        Capsule::DataBlocked(u64::MAX),
        Capsule::MaxStreamsBidi(MAX_STREAMS + 1),
        Capsule::MaxStreamsUni(MAX_STREAMS + 1),
        Capsule::StreamsBlockedBidi(MAX_STREAMS + 1),
        Capsule::StreamsBlockedUni(MAX_STREAMS + 1),
    ] {
        assert_eq!(encode(&capsule), Err(Error::ValueOutOfRange));
    }
}

#[test]
fn public_varint_prefix_codec() {
    use graphite_meter_core::capsule::{decode_varint, encode_varint};
    for value in [0, 63, 64, 16383, 16384, (1 << 30) - 1, 1 << 30, MAX_VARINT] {
        let mut bytes = Vec::new();
        encode_varint(value, &mut bytes).unwrap();
        for end in 0..bytes.len() {
            assert_eq!(decode_varint(&bytes[..end]), Ok(None));
        }
        assert_eq!(decode_varint(&bytes), Ok(Some((value, bytes.len()))));
        assert_eq!(decode_varint(&varint(value)), Ok(Some((value, 8))));
    }
    let mut bytes = vec![1];
    assert_eq!(
        encode_varint(MAX_VARINT + 1, &mut bytes),
        Err(Error::ValueOutOfRange)
    );
    assert_eq!(bytes, [1]);
}

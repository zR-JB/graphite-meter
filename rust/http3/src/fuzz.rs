//! Fuzz target bodies for the libFuzzer targets in `fuzz/`.
use crate::{
    Code, WtCode, capsule, control, fields, frame,
    message::{Event, Message},
    qpack, settings, varint,
};
use bytes::Bytes;

/// Frames read in arbitrary chunks equal the frames read in one piece.
pub fn frames(data: &[u8]) {
    let Some((&seed, data)) = data.split_first() else {
        return;
    };
    let read = |chunk: &dyn Fn(usize) -> usize| {
        let mut reader = frame::Reader::default();
        let mut frames: Vec<(u64, u64, Vec<u8>)> = Vec::new();
        for mut input in chunks(data, chunk) {
            while let Some(piece) = reader.next(&mut input) {
                match piece {
                    frame::Piece::Header { kind, length } => frames.push((kind, length, Vec::new())),
                    frame::Piece::Payload(payload) => {
                        let (_, length, received) = frames.last_mut().expect("payload follows a header");
                        received.extend_from_slice(&payload);
                        assert!(received.len() as u64 <= *length);
                    }
                }
            }
            assert!(input.is_empty());
        }
        (frames, reader.at_boundary())
    };
    assert_eq!(
        read(&|_| data.len()),
        read(&|index| (index * usize::from(seed)) % 23 + 1)
    );
}

/// SETTINGS read in arbitrary chunks give the same peer or error; the dialect decision is total.
pub fn settings(data: &[u8]) {
    let Some((&seed, payload)) = data.split_first() else {
        return;
    };
    let read = |chunk: &dyn Fn(usize) -> usize| {
        let mut reader = settings::Reader::new(payload.len() as u64)?;
        let mut peer = reader.read(&mut &payload[..0])?;
        for input in chunks(payload, chunk) {
            peer = reader.read(&mut &input[..])?;
        }
        Ok(peer.expect("the whole payload was read"))
    };
    let whole: Result<settings::Peer, crate::Code> = read(&|_| payload.len());
    assert_eq!(whole, read(&|index| (index * usize::from(seed)) % 7 + 1));
    if let Ok(peer) = whole {
        assert_eq!(peer.webtransport(false), None);
        let _ = peer.webtransport(true);
    }
}

/// A peer's control stream read in arbitrary chunks agrees with a whole-input model of its
/// sequencing, for either role: SETTINGS first, the frames a peer may send, and the values of
/// GOAWAY, MAX_PUSH_ID and CANCEL_PUSH.
pub fn control(data: &[u8]) {
    let Some((&seed, data)) = data.split_first() else {
        return;
    };
    let client = seed & 1 == 1;
    let read = |chunk: &dyn Fn(usize) -> usize| {
        let mut reader = control::Reader::default();
        let mut events = Vec::new();
        for mut input in chunks(data, chunk) {
            let read = reader.read(client, &mut input, |event| {
                events.push(event);
                Ok(())
            });
            if let Err(code) = read {
                return (events, Err(code));
            }
            assert!(input.is_empty());
        }
        (events, Ok(()))
    };
    let model = control_model(data, client);
    assert_eq!(read(&|_| data.len()), model);
    assert_eq!(read(&|index| (index * usize::from(seed >> 1)) % 11 + 1), model);
}

/// RFC 9114 §6.2.1 and §7.2 over a whole control stream, independent of the incremental reader;
/// where the stream stops short, what came before stands.
fn control_model(mut data: &[u8], client: bool) -> (Vec<control::Event>, Result<(), Code>) {
    let (mut events, mut first, mut goaway) = (Vec::new(), true, None);
    loop {
        let Some(((kind, a), (length, b))) = varint::decode(data).and_then(|(kind, a)| {
            let length = varint::decode(&data[a..])?;
            Some(((kind, a), length))
        }) else {
            return (events, Ok(()));
        };
        data = &data[a + b..];
        let refusal = match (kind, std::mem::replace(&mut first, false)) {
            (0x04, true) if length > 8 * 1024 => Some(Code::H3_EXCESSIVE_LOAD),
            (0x04, true) => None,
            (_, true) => Some(Code::H3_MISSING_SETTINGS),
            (0x0d, false) if client => Some(Code::H3_FRAME_UNEXPECTED),
            (0x03 | 0x07 | 0x0d, false) if !(1..=8).contains(&length) => Some(Code::H3_FRAME_ERROR),
            (0x00..=0x02 | 0x04..=0x06 | 0x08 | 0x09, false) => Some(Code::H3_FRAME_UNEXPECTED),
            _ => None,
        };
        if let Some(code) = refusal {
            return (events, Err(code));
        }
        let available = usize::try_from(length).map_or(data.len(), |length| length.min(data.len()));
        let (payload, rest) = data.split_at(available);
        data = rest;
        let whole = payload.len() as u64 == length;
        match kind {
            0x04 => match settings::Reader::new(length).and_then(|mut reader| reader.read(&mut &payload[..])) {
                Err(code) => return (events, Err(code)),
                Ok(Some(peer)) => events.push(control::Event::Settings(peer)),
                Ok(None) => {}
            },
            0x03 | 0x07 | 0x0d if whole => {
                let value = match varint::decode(payload) {
                    Some((value, size)) if size == payload.len() => value,
                    _ => return (events, Err(Code::H3_FRAME_ERROR)),
                };
                if kind == 0x07 && client {
                    if !value.is_multiple_of(4) || goaway.is_some_and(|previous| value > previous) {
                        return (events, Err(Code::H3_ID_ERROR));
                    }
                    goaway = Some(value);
                    events.push(control::Event::Goaway(value));
                }
            }
            _ => {}
        }
        if !whole {
            return (events, Ok(()));
        }
    }
}

/// A peer's QPACK encoder and decoder streams read in arbitrary chunks agree with a whole-input model
/// for a peer that may use no dynamic table, as our SETTINGS allow none.
pub fn qpack_streams(data: &[u8]) {
    let Some((&seed, data)) = data.split_first() else {
        return;
    };
    let (encoder, decoder) = instructions(data);
    let chunked = |index: usize| (index * usize::from(seed)) % 7 + 1;
    assert_eq!(qpack::encoder_stream(data), encoder);
    assert_eq!(
        chunks(data, &chunked).try_for_each(|chunk| qpack::encoder_stream(&chunk)),
        encoder
    );
    assert_eq!(qpack::DecoderStream::default().read(data), decoder);
    let mut split = qpack::DecoderStream::default();
    assert_eq!(chunks(data, &chunked).try_for_each(|chunk| split.read(&chunk)), decoder);
}

/// RFC 9204 §4.3 and §4.4 over whole streams: of the encoder's instructions only Set Dynamic Table
/// Capacity 0 fits, and of the decoder's only Stream Cancellation, as the others name inserts.
fn instructions(data: &[u8]) -> (Result<(), Code>, Result<(), Code>) {
    // Where the prefixed integer (RFC 7541 §5.1) that `data` starts with ends, unless past `data`.
    let end = |data: &[u8], bits: u32| {
        let max = (1 << bits) - 1;
        if data[0] & max != max {
            return Some(1);
        }
        data[1..].iter().position(|&byte| byte & 0x80 == 0).map(|last| last + 2)
    };
    let mut rest = data;
    let encoder = loop {
        match rest.first() {
            None => break Ok(()),
            // Set Dynamic Table Capacity, 001 and a 5-bit prefix, to 0.
            Some(&first) if first & 0xe0 == 0x20 && first & 0x1f == 0 => {
                rest = &rest[end(rest, 5).expect("a capacity within its prefix")..];
            }
            Some(_) => break Err(Code::QPACK_ENCODER_STREAM_ERROR),
        }
    };
    let mut rest = data;
    let decoder = loop {
        match rest.first() {
            None => break Ok(()),
            // Stream Cancellation, 01 and a 6-bit prefix; its stream ID may run past the input.
            Some(&first) if first & 0xc0 == 0x40 => match end(rest, 6) {
                Some(used) => rest = &rest[used..],
                None => break Ok(()),
            },
            Some(_) => break Err(Code::QPACK_DECODER_STREAM_ERROR),
        }
    };
    (encoder, decoder)
}

/// QPACK decoding stays within the field limit, and re-encoding decoded lines decodes to the same lines.
pub fn qpack(data: &[u8]) {
    let lines = |section: &[u8]| {
        let mut lines = Vec::new();
        qpack::decode(section, |name, value| {
            lines.push((name.to_vec(), value.to_vec()));
            Ok(())
        })
        .map(|()| lines)
    };
    if let Ok(decoded) = lines(data) {
        let mut encoded = Vec::new();
        qpack::encode(
            decoded.iter().map(|(name, value)| (&name[..], &value[..])),
            &mut encoded,
        );
        assert_eq!(lines(&encoded), Ok(decoded));
    }
    for limit in [0, 128, 4096] {
        if let Ok(head) = fields::decode_request(data, limit) {
            assert!(head.size <= limit);
        }
        if let Ok(head) = fields::decode_response(data, limit) {
            assert!(head.size <= limit);
        }
        let _ = fields::check_trailers(data, limit);
    }
}

/// Huffman encoding round-trips, and a valid encoding is the unique encoding of what it decodes to.
pub fn huffman(data: &[u8]) {
    let mut encoded = Vec::new();
    qpack::huffman::encode(data, &mut encoded);
    assert_eq!(encoded.len(), qpack::huffman::encoded_len(data));
    let mut decoded = Vec::new();
    qpack::huffman::decode(&encoded, &mut decoded).expect("our encoding decodes");
    assert_eq!(decoded, data);
    decoded.clear();
    if qpack::huffman::decode(data, &mut decoded).is_ok() {
        encoded.clear();
        qpack::huffman::encode(&decoded, &mut encoded);
        assert_eq!(encoded, data);
    }
}

/// Capsules read in arbitrary chunks equal those read in one piece, errors included.
pub fn capsules(data: &[u8]) {
    let Some((&seed, data)) = data.split_first() else {
        return;
    };
    let read = |chunk: &dyn Fn(usize) -> usize| {
        let mut reader = capsule::Reader::default();
        let mut capsules = Vec::new();
        for mut input in chunks(data, chunk) {
            while let Some(capsule) = reader.read(&mut input)? {
                capsules.push(capsule);
            }
        }
        Ok::<_, crate::Code>((capsules, reader.at_boundary()))
    };
    assert_eq!(
        read(&|_| data.len()),
        read(&|index| (index * usize::from(seed)) % 13 + 1)
    );
}

/// A stream's type and session read byte by byte match one read, and datagrams route to CONNECT stream IDs.
pub fn webtransport_ids(data: &[u8]) {
    let read = |chunk: &dyn Fn(usize) -> usize| {
        let mut header = frame::StreamType::default();
        let mut consumed = 0;
        for mut input in chunks(data, chunk) {
            let before = input.len();
            let result = header.read(&mut input);
            consumed += before - input.len();
            if result.is_some() {
                return (result, consumed);
            }
        }
        (None, consumed)
    };
    assert_eq!(read(&|_| data.len()), read(&|_| 1));
    if let Ok((session, payload)) = capsule::datagram(Bytes::copy_from_slice(data)) {
        assert!(session.is_multiple_of(4) && session < 1 << 62 && payload.len() < data.len());
    }
}

/// A request stream read in arbitrary chunks agrees with a whole-input reference model.
pub fn request_stream(data: &[u8]) {
    let [limit, length, data @ ..] = data else {
        return;
    };
    let limit = u64::from(*limit) * 4;
    let length = (*length != 0xff).then_some(u64::from(*length));
    let read = |chunk: &dyn Fn(usize) -> usize| {
        let mut message = Message::new(limit, false);
        let mut events = Vec::new();
        let mut result = Ok(());
        'chunks: for mut input in chunks(data, chunk) {
            loop {
                match message.next(&mut input) {
                    Ok(Some(event)) => {
                        if matches!(event, Event::Head(_)) {
                            message.content_length(length);
                        }
                        push(&mut events, event);
                    }
                    Ok(None) => break,
                    Err(code) => {
                        result = Err(code);
                        break 'chunks;
                    }
                }
            }
        }
        let result = result.and_then(|()| message.finish());
        (events, result)
    };
    let (model_events, model_result) = model(data, limit, length);
    let whole = |_| data.len();
    let split = |index: usize| (index * 7 + data.len()) % 11 + 1;
    for chunking in [&whole as &dyn Fn(usize) -> usize, &split] {
        let (events, result) = read(chunking);
        assert_eq!(result, model_result);
        // Data before an error depends on where chunks end; field sections do not.
        let fields = |events: &[(u8, Vec<u8>)]| {
            events
                .iter()
                .filter(|(tag, _)| *tag != b'd')
                .cloned()
                .collect::<Vec<_>>()
        };
        if result.is_ok() {
            assert_eq!(events, model_events);
        } else {
            assert_eq!(fields(&events), fields(&model_events));
        }
    }
}

/// Events tagged `h`, `d` or `t`, with consecutive DATA merged.
type Events = Vec<(u8, Vec<u8>)>;

fn push(events: &mut Events, event: Event) {
    let (tag, bytes) = match event {
        Event::Head(bytes) => (b'h', bytes),
        Event::Data(bytes) if bytes.is_empty() => return,
        Event::Data(bytes) => (b'd', bytes),
        Event::Trailers(bytes) => (b't', bytes),
    };
    match events.last_mut() {
        Some((b'd', data)) if tag == b'd' => data.extend_from_slice(&bytes),
        _ => events.push((tag, bytes.to_vec())),
    }
}

/// RFC 9114 §4.1 over the whole stream at once, independent of the incremental reader.
fn model(mut data: &[u8], limit: u64, length: Option<u64>) -> (Events, Result<(), Code>) {
    let (mut events, mut phase, mut owed) = (Vec::new(), 0, None);
    loop {
        let Some((kind, a)) = varint::decode(data) else {
            let end = match (data.is_empty(), phase, owed) {
                (false, ..) => Err(Code::H3_FRAME_ERROR),
                (true, 0, _) => Err(Code::H3_REQUEST_INCOMPLETE),
                (true, _, Some(owed)) if owed > 0 => Err(Code::H3_MESSAGE_ERROR),
                _ => Ok(()),
            };
            return (events, end);
        };
        let Some((length_field, b)) = varint::decode(&data[a..]) else {
            return (events, Err(Code::H3_FRAME_ERROR));
        };
        data = &data[a + b..];
        match (kind, phase) {
            (0x01, 0 | 1) if length_field > limit => return (events, Err(Code::H3_EXCESSIVE_LOAD)),
            (0x01, 0 | 1) | (0x00, 1) => {}
            (0x41, 0) => return (events, Err(WtCode(0).to_http())),
            (0x00..=0x09 | 0x0d, _) => return (events, Err(Code::H3_FRAME_UNEXPECTED)),
            _ => {}
        }
        let payload = &data[..usize::try_from(length_field).map_or(data.len(), |length| length.min(data.len()))];
        data = &data[payload.len()..];
        if kind == 0x00 {
            if let Some(remaining) = owed {
                if payload.len() as u64 > remaining {
                    return (events, Err(Code::H3_MESSAGE_ERROR));
                }
                owed = Some(remaining - payload.len() as u64);
            }
            push(&mut events, Event::Data(Bytes::copy_from_slice(payload)));
        }
        if (payload.len() as u64) < length_field {
            return (events, Err(Code::H3_FRAME_ERROR));
        }
        if kind == 0x01 {
            let section = Bytes::copy_from_slice(payload);
            push(
                &mut events,
                if phase == 0 {
                    Event::Head(section)
                } else {
                    Event::Trailers(section)
                },
            );
            (phase, owed) = (phase + 1, if phase == 0 { length } else { owed });
        }
    }
}

/// Splits `data` into chunks whose lengths `chunk` picks by index; each at least one byte.
fn chunks<'a>(data: &'a [u8], chunk: &'a dyn Fn(usize) -> usize) -> impl Iterator<Item = Bytes> + 'a {
    let mut rest = Bytes::copy_from_slice(data);
    (0..).map_while(move |index| (!rest.is_empty()).then(|| rest.split_to(chunk(index).clamp(1, rest.len()))))
}

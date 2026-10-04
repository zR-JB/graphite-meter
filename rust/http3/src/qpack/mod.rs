//! Static-only QPACK (RFC 9204): capacity 0 is advertised by omission, so no dynamic references exist.
pub(crate) mod huffman;
mod table;

use crate::code::Code;

/// Why a field section was refused; the stream answers each differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Invalid {
    /// QPACK_DECOMPRESSION_FAILED.
    Qpack,
    /// H3_MESSAGE_ERROR.
    Malformed,
    /// Over the size limit: a 431 response to requests.
    TooLarge,
    /// Well formed, but not a request our routes serve: a 400 response.
    Unsupported,
}

impl Invalid {
    /// The code a stream that cannot answer with a status ends with.
    pub(crate) fn code(self) -> Code {
        match self {
            Self::Qpack => Code::QPACK_DECOMPRESSION_FAILED,
            Self::TooLarge => Code::H3_EXCESSIVE_LOAD,
            Self::Malformed | Self::Unsupported => Code::H3_MESSAGE_ERROR,
        }
    }
}

/// Calls `field` for each line of an encoded field section, in order.
pub(crate) fn decode(
    section: &[u8],
    mut field: impl FnMut(&[u8], &[u8]) -> Result<(), Invalid>,
) -> Result<(), Invalid> {
    // Required Insert Count 0 and Base 0.
    let mut section = section.strip_prefix(&[0, 0]).ok_or(Invalid::Qpack)?;
    let (mut name_buffer, mut value_buffer) = (Vec::new(), Vec::new());
    while let Some(&first) = section.first() {
        match first {
            // Indexed field line; T=0 would reference the dynamic table.
            0xc0.. => {
                let (name, value) = entry(integer(&mut section, 6)?)?;
                field(name.as_bytes(), value.as_bytes())?;
            }
            // Literal field line with a static name reference.
            0x50..=0x5f | 0x70..=0x7f => {
                let (name, _) = entry(integer(&mut section, 4)?)?;
                let value = string(&mut section, 7, &mut value_buffer)?;
                field(name.as_bytes(), value)?;
            }
            // Literal field line with a literal name.
            0x20..=0x3f => {
                let name = string(&mut section, 3, &mut name_buffer)?;
                let value = string(&mut section, 7, &mut value_buffer)?;
                field(name, value)?;
            }
            _ => return Err(Invalid::Qpack),
        }
    }
    Ok(())
}

/// Appends a field section, using static references where they exist and Huffman where it is shorter.
pub(crate) fn encode<'a>(fields: impl IntoIterator<Item = (&'a [u8], &'a [u8])>, output: &mut Vec<u8>) {
    output.extend_from_slice(&[0, 0]);
    for (name, value) in fields {
        match table::find(name, value) {
            table::Match::Field(index) => put_integer(index, 6, 0xc0, output),
            table::Match::Name(index) => {
                put_integer(index, 4, 0x50, output);
                put_string(value, 7, 0x00, output);
            }
            table::Match::None => {
                put_string(name, 3, 0x20, output);
                put_string(value, 7, 0x00, output);
            }
        }
    }
}

/// The peer's encoder stream (RFC 9204 §4.3): only Set Dynamic Table Capacity 0 fits the capacity
/// our SETTINGS allow.
pub(crate) fn encoder_stream(input: &[u8]) -> Result<(), Code> {
    match input.iter().all(|&byte| byte == 0x20) {
        true => Ok(()),
        false => Err(Code::QPACK_ENCODER_STREAM_ERROR),
    }
}

/// The peer's decoder stream (RFC 9204 §4.4), read in chunks: only Stream Cancellation fits an
/// encoder that never inserts.
#[derive(Default)]
pub(crate) struct DecoderStream {
    /// Within a Stream Cancellation's stream ID.
    continuing: bool,
}

impl DecoderStream {
    pub(crate) fn read(&mut self, input: &[u8]) -> Result<(), Code> {
        for &byte in input {
            if self.continuing {
                self.continuing = byte & 0x80 != 0;
            } else if byte & 0xc0 == 0x40 {
                self.continuing = byte & 0x3f == 0x3f;
            } else {
                return Err(Code::QPACK_DECODER_STREAM_ERROR);
            }
        }
        Ok(())
    }
}

fn entry(index: u64) -> Result<(&'static str, &'static str), Invalid> {
    usize::try_from(index)
        .ok()
        .and_then(|index| table::STATIC.get(index))
        .copied()
        .ok_or(Invalid::Qpack)
}

/// A prefixed integer (RFC 7541 §5.1) whose value fits the section anyway, so at most 2^35.
fn integer(section: &mut &[u8], bits: u32) -> Result<u64, Invalid> {
    let (&first, mut rest) = section.split_first().ok_or(Invalid::Qpack)?;
    let max = (1 << bits) - 1;
    let mut value = u64::from(first) & max;
    if value == max {
        for shift in (0..=28).step_by(7) {
            let (&byte, tail) = rest.split_first().ok_or(Invalid::Qpack)?;
            rest = tail;
            value += u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                *section = rest;
                return Ok(value);
            }
        }
        return Err(Invalid::Qpack);
    }
    *section = rest;
    Ok(value)
}

/// A string literal after a prefix of `bits` length bits and one Huffman bit.
fn string<'a: 'b, 'b>(section: &mut &'a [u8], bits: u32, buffer: &'b mut Vec<u8>) -> Result<&'b [u8], Invalid> {
    let huffman = section.first().is_some_and(|&first| first & 1 << bits != 0);
    let length = usize::try_from(integer(section, bits)?).map_err(|_| Invalid::Qpack)?;
    let (raw, rest) = section.split_at_checked(length).ok_or(Invalid::Qpack)?;
    *section = rest;
    if !huffman {
        return Ok(raw);
    }
    buffer.clear();
    huffman::decode(raw, buffer)?;
    Ok(buffer)
}

fn put_integer(value: u64, bits: u32, pattern: u8, output: &mut Vec<u8>) {
    let max = (1 << bits) - 1;
    if value < max {
        output.push(pattern | value as u8);
        return;
    }
    output.push(pattern | max as u8);
    let mut rest = value - max;
    while rest >= 0x80 {
        output.push(0x80 | rest as u8);
        rest >>= 7;
    }
    output.push(rest as u8);
}

fn put_string(text: &[u8], bits: u32, pattern: u8, output: &mut Vec<u8>) {
    let huffman = huffman::encoded_len(text);
    if huffman < text.len() {
        put_integer(huffman as u64, bits, pattern | 1 << bits, output);
        huffman::encode(text, output);
    } else {
        put_integer(text.len() as u64, bits, pattern, output);
        output.extend_from_slice(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    type Lines = Vec<(Vec<u8>, Vec<u8>)>;

    fn fields(section: &[u8]) -> Result<Lines, Invalid> {
        let mut fields = Vec::new();
        decode(section, |name, value| {
            fields.push((name.to_vec(), value.to_vec()));
            Ok(())
        })?;
        Ok(fields)
    }

    fn unescape(text: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut rest = text.as_bytes();
        while let Some((&byte, tail)) = rest.split_first() {
            (bytes, rest) = match (byte, tail) {
                (b'\\', [b'\\', tail @ ..]) => ([bytes, vec![b'\\']].concat(), tail),
                (b'\\', [b'x', high, low, tail @ ..]) => {
                    let code = u8::from_str_radix(std::str::from_utf8(&[*high, *low]).unwrap(), 16).unwrap();
                    ([bytes, vec![code]].concat(), tail)
                }
                _ => ([bytes, vec![byte]].concat(), tail),
            };
        }
        bytes
    }

    #[test]
    fn go_and_rust_decode_each_other() {
        let mut block = Vec::new();
        let mut go = Vec::new();
        for line in include_str!("vectors.txt")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            if let Some(encoded) = line.strip_prefix("go: ") {
                go = hex(encoded);
            } else if let Some(encoded) = line.strip_prefix("rust: ") {
                assert_eq!(fields(&go), Ok(block.clone()), "Rust decodes Go's encoding");
                let mut ours = Vec::new();
                encode(block.iter().map(|(name, value)| (name.as_slice(), value.as_slice())), &mut ours);
                let ours_hex: String = ours.iter().map(|byte| format!("{byte:02x}")).collect();
                assert_eq!(encoded, ours_hex, "our encoding changed: recheck Go decodes it");
                assert_eq!(fields(&ours), Ok(block.clone()));
                block.clear();
            } else {
                let (name, value) = line.split_once(' ').unwrap_or((line, ""));
                block.push((unescape(name), unescape(value)));
            }
        }
    }

    #[test]
    fn rfc9204_static_reference() {
        assert_eq!(
            fields(&hex("0000510b2f696e6465782e68746d6c")),
            Ok(vec![(b":path".to_vec(), b"/index.html".to_vec())])
        );
    }

    #[test]
    fn dynamic_references_and_malformed_sections_fail() {
        for section in [
            "",
            "00",
            "0100d1",
            "0001d1",
            "0080d1",
            "000080",
            "000010",
            "00004f0161",
            "00000f0161",
            "0000ff24",
            "0000ffffffffffff7f",
            "0000510a2f",
            "0000508a",
            "0000518100",
            "00005184ffffffff",
            "000021",
        ] {
            assert_eq!(fields(&hex(section)), Err(Invalid::Qpack), "{section}");
        }
        assert_eq!(fields(&hex("0000")), Ok(vec![]));
    }
}

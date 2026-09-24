//! Attribute proxy headers only through an explicitly trusted socket peer.
use std::net::{IpAddr, SocketAddr};

use graphite_meter_core::discovery::ClientIpSource;
use http::HeaderMap;
use ipnet::IpNet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientAddress {
    pub addr: IpAddr,
    pub source: ClientIpSource,
}
impl ClientAddress {
    pub fn version(self) -> u8 {
        if self.addr.is_ipv4() { 4 } else { 6 }
    }

    /// Anonymous clients share an IPv6 /64 budget despite rotating addresses.
    /// Authenticated policy replaces this key with the verified subject identity.
    pub fn anonymous_key(self) -> String {
        match self.addr.to_canonical() {
            IpAddr::V4(address) => address.to_string(),
            IpAddr::V6(address) => ipnet::Ipv6Net::new(address, 64)
                .expect("fixed valid IPv6 prefix")
                .trunc()
                .to_string(),
        }
    }
}

pub fn resolve(peer: SocketAddr, headers: &HeaderMap, trusted: &[IpNet]) -> ClientAddress {
    let peer = unmap(peer.ip());
    let socket = ClientAddress {
        addr: peer,
        source: ClientIpSource::Socket,
    };
    if !contains(trusted, peer) {
        return socket;
    }
    let Some(chain) = forwarded_chain(headers) else {
        return socket;
    };
    let mut current = peer;
    for hop in chain.into_iter().rev() {
        if !contains(trusted, current) {
            break;
        }
        current = hop;
    }
    ClientAddress {
        addr: current,
        source: ClientIpSource::Forwarded,
    }
}

fn contains(trusted: &[IpNet], addr: IpAddr) -> bool {
    trusted.iter().any(|prefix| prefix.contains(&addr))
}
fn unmap(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(addr),
        _ => addr,
    }
}
fn forwarded_chain(headers: &HeaderMap) -> Option<Vec<IpAddr>> {
    // X-Real-IP is a singleton. Forwarded and X-Forwarded-For are lists: a
    // proxy may append a second field line to a client-supplied first one, so
    // parse every line and walk the complete chain from the trusted socket.
    for name in ["x-real-ip", "forwarded", "x-forwarded-for"] {
        let mut values = headers.get_all(name).iter();
        let Some(value) = values.next() else {
            continue;
        };
        let first = value.to_str().ok()?;
        let second = values.next();
        if name == "x-real-ip" && second.is_some() {
            return None;
        }
        let joined;
        let raw = if let Some(second) = second {
            let mut raw = first.to_owned();
            raw.push(',');
            raw.push_str(second.to_str().ok()?);
            for value in values {
                raw.push(',');
                raw.push_str(value.to_str().ok()?);
            }
            joined = raw;
            joined.as_str()
        } else {
            first
        };
        if raw.is_empty() {
            continue;
        }
        return match name {
            "x-real-ip" => Some(vec![parse_address(raw)?]),
            "forwarded" => split_quoted(raw, b',')?
                .into_iter()
                .map(|element| {
                    let params = split_quoted(element, b';')?;
                    let value = params.into_iter().find_map(|param| {
                        let (key, value) = param.split_once('=')?;
                        key.trim()
                            .eq_ignore_ascii_case("for")
                            .then_some(value.trim())
                    })?;
                    parse_address(value)
                })
                .collect(),
            _ => raw.split(',').map(parse_address).collect(),
        };
    }
    None
}

fn parse_address(raw: &str) -> Option<IpAddr> {
    let raw = raw.trim();
    let decoded;
    let raw = if raw.len() >= 2 && raw.starts_with('"') {
        decoded = unquote(raw)?;
        decoded.as_str()
    } else {
        raw
    };
    // IpAddr cannot retain scoped IPv6 zones. Reject rather than erase zone
    // information and accidentally grant trust to a different address.
    if raw.contains('%') {
        return None;
    }
    if let Ok(ip) = raw.parse::<IpAddr>() {
        return Some(unmap(ip));
    }
    if let Some(inner) = raw.strip_prefix('[').and_then(|v| v.strip_suffix(']'))
        && let Ok(ip) = inner.parse::<IpAddr>()
    {
        return Some(unmap(ip));
    }
    raw.parse::<SocketAddr>().ok().map(|addr| unmap(addr.ip()))
}

fn split_quoted(raw: &str, separator: u8) -> Option<Vec<&str>> {
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    let mut parts = Vec::new();
    for (index, byte) in raw.bytes().enumerate() {
        if escaped {
            escaped = false;
        } else if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if byte == separator && !quoted {
            parts.push(raw[start..index].trim());
            start = index + 1;
        }
    }
    if quoted || escaped {
        return None;
    }
    parts.push(raw[start..].trim());
    Some(parts)
}

// strconv.Unquote's double-quoted escape forms. Decoded non-UTF8 bytes cannot
// represent an IP address and are rejected. Input storage is bounded by HTTP's
// configured header limit; there is no separate hop cutoff.
fn unquote(raw: &str) -> Option<String> {
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    let mut chars = inner.chars();
    let mut output = Vec::with_capacity(inner.len());
    while let Some(ch) = chars.next() {
        if ch == '"' || ch == '\n' {
            return None;
        }
        if ch != '\\' {
            output.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
            continue;
        }
        let escape = chars.next()?;
        let byte = match escape {
            'a' => 7,
            'b' => 8,
            'f' => 12,
            'n' => 10,
            'r' => 13,
            't' => 9,
            'v' => 11,
            '\\' => b'\\',
            '"' => b'"',
            'x' | 'u' | 'U' => {
                let count = match escape {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let mut value = 0_u32;
                for _ in 0..count {
                    value = value
                        .checked_mul(16)?
                        .checked_add(chars.next()?.to_digit(16)?)?;
                }
                if escape == 'x' {
                    output.push(value as u8);
                } else {
                    output.extend_from_slice(
                        char::from_u32(value)?.encode_utf8(&mut [0; 4]).as_bytes(),
                    );
                }
                continue;
            }
            '0'..='7' => {
                let mut value = escape.to_digit(8)?;
                for _ in 0..2 {
                    value = value * 8 + chars.next()?.to_digit(8)?;
                }
                u8::try_from(value).ok()?
            }
            _ => return None,
        };
        output.push(byte);
    }
    String::from_utf8(output).ok()
}

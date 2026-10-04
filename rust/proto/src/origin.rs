//! HTTP(S) origins in canonical form: the form browsers write in an `Origin` header.

use crate::idna;
use serde::{Serialize, Serializer};
use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

/// The longest origin text a contract accepts.
pub const MAX_ORIGIN_BYTES: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    pub const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// An origin's host: a lowercase ASCII name or an IP address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Host {
    Name(String),
    Ip(IpAddr),
}

impl Host {
    /// A name of ASCII letters, digits, `-` and `_` labels, or a dotted IPv4 address if it ends in a number.
    fn parse(text: &str) -> Result<Self, OriginError> {
        let name = text.strip_suffix('.').unwrap_or(text);
        let label_byte = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_');
        if !name
            .split('.')
            .all(|label| !label.is_empty() && label.bytes().all(label_byte))
        {
            return Err(OriginError);
        }
        let last = name.rsplit('.').next().unwrap_or_default();
        let hex = last.strip_prefix("0x").or_else(|| last.strip_prefix("0X"));
        let numeric = last.bytes().all(|byte| byte.is_ascii_digit())
            || hex.is_some_and(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
        if numeric {
            let ip: Ipv4Addr = text.parse().map_err(|_| OriginError)?;
            return Ok(Self::Ip(ip.into()));
        }
        Ok(Self::Name(text.to_ascii_lowercase()))
    }
}

impl fmt::Display for Host {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => formatter.write_str(name),
            Self::Ip(IpAddr::V4(ip)) => write!(formatter, "{ip}"),
            Self::Ip(IpAddr::V6(ip)) => write!(formatter, "[{ip}]"),
        }
    }
}

/// An HTTP(S) origin; two spellings of one origin parse equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Origin {
    pub scheme: Scheme,
    pub host: Host,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OriginError;

impl fmt::Display for OriginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected an HTTP(S) origin with an ASCII host and no credentials, path, query or fragment")
    }
}

impl std::error::Error for OriginError {}

impl Origin {
    /// Parses an origin of at most 2048 bytes: an ASCII host, a nonzero port, no credentials, path, query or
    /// fragment.
    pub fn parse(text: &str) -> Result<Self, OriginError> {
        match Self::split(text)? {
            (origin, "") if text.len() <= MAX_ORIGIN_BYTES => Ok(origin),
            _ => Err(OriginError),
        }
    }

    /// Parses an origin received in a catalogue or preflight, whose international host becomes the punycode Go
    /// dials.
    pub fn parse_received(text: &str) -> Result<Self, OriginError> {
        if text.is_ascii() {
            return Self::parse(text);
        }
        if text.len() > MAX_ORIGIN_BYTES {
            return Err(OriginError);
        }
        let (scheme, authority) = text.split_once("://").ok_or(OriginError)?;
        let (host, port) = authority.split_at(authority.find(':').unwrap_or(authority.len()));
        let host = idna::to_ascii(host).ok_or(OriginError)?;
        Self::parse(&format!("{scheme}://{host}{port}"))
    }

    /// Splits a URL into its origin and the rest, which begins at its path, query or fragment.
    pub fn split(url: &str) -> Result<(Self, &str), OriginError> {
        let (scheme, rest) = url.split_once("://").ok_or(OriginError)?;
        let scheme = [Scheme::Http, Scheme::Https]
            .into_iter()
            .find(|known| known.name().eq_ignore_ascii_case(scheme))
            .ok_or(OriginError)?;
        let (authority, rest) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
        let (host, port) = match authority.strip_prefix('[') {
            Some(literal) => {
                let (ip, port) = literal.split_once(']').ok_or(OriginError)?;
                (Host::Ip(ip.parse::<Ipv6Addr>().map_err(|_| OriginError)?.into()), port)
            }
            None => {
                let (name, port) = authority.split_at(authority.find(':').unwrap_or(authority.len()));
                (Host::parse(name)?, port)
            }
        };
        let port = match port {
            "" | ":" => scheme.default_port(),
            _ => port.strip_prefix(':').and_then(decimal_port).ok_or(OriginError)?,
        };
        Ok((Self { scheme, host, port }, rest))
    }
}

/// A nonzero port of decimal digits only.
fn decimal_port(digits: &str) -> Option<u16> {
    let port: u16 = digits
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| digits.parse().ok())??;
    (port != 0).then_some(port)
}

impl fmt::Display for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}://{}", self.scheme.name(), self.host)?;
        if self.port != self.scheme.default_port() {
            write!(formatter, ":{}", self.port)?;
        }
        Ok(())
    }
}

impl Serialize for Origin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// A received target's `baseUrl` or catalogue `url`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BaseUrl {
    /// `.`: the origin that served the document.
    Served,
    Origin(Origin),
}

impl BaseUrl {
    pub fn parse(text: &str) -> Result<Self, OriginError> {
        match text {
            "." => Ok(Self::Served),
            _ => Origin::parse_received(text).map(Self::Origin),
        }
    }

    /// The origin this names, given the origin that served the document.
    pub fn resolve<'a>(&'a self, served: &'a Origin) -> &'a Origin {
        match self {
            Self::Served => served,
            Self::Origin(origin) => origin,
        }
    }
}

impl Serialize for BaseUrl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Served => serializer.serialize_str("."),
            Self::Origin(origin) => origin.serialize(serializer),
        }
    }
}

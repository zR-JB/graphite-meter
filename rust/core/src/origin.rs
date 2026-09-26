//! Origin identities preserve explicit port and IPv6 spelling used by Go catalogs.
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub scheme: String,
    pub host: String,
    pub port: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OriginError;
impl fmt::Display for OriginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected an absolute HTTP(S) origin")
    }
}
impl std::error::Error for OriginError {}

impl Origin {
    pub fn port_number(&self) -> u16 {
        self.port
            .as_deref()
            .map_or(if self.scheme == "https" { 443 } else { 80 }, |port| {
                port.parse().expect("validated origin port")
            })
    }

    pub fn authority(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        match &self.port {
            Some(port) => format!("{host}:{port}"),
            None => host,
        }
    }

    pub fn key(&self) -> String {
        let mut normalized = self.clone();
        normalized.scheme.make_ascii_lowercase();
        normalized.host.make_ascii_lowercase();
        if matches!(
            (normalized.scheme.as_str(), normalized.port.as_deref()),
            ("http", Some("80")) | ("https", Some("443"))
        ) {
            normalized.port = None;
        }
        format!("{}://{}", normalized.scheme, normalized.authority())
    }
}

/// A relative self target is represented explicitly, never as an empty hostname.
pub fn target_origin(raw: &str) -> Result<Option<Origin>, OriginError> {
    if raw == "." {
        return Ok(None);
    }
    if raw.len() > 2048
        || raw
            .bytes()
            .any(|c| c <= b' ' || c == 127 || matches!(c, b'\\' | b'#' | b'?'))
    {
        return Err(OriginError);
    }
    let (scheme, authority) = raw.split_once("://").ok_or(OriginError)?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") || authority.contains(['/', '@']) {
        return Err(OriginError);
    }
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, suffix) = bracketed.split_once(']').ok_or(OriginError)?;
        host.parse::<std::net::Ipv6Addr>()
            .map_err(|_| OriginError)?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or(OriginError)?)
        };
        (host, port)
    } else {
        let (host, port) = authority
            .split_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)));
        if !ascii_name(host) {
            return Err(OriginError);
        }
        (host, port)
    };
    if host.is_empty() {
        return Err(OriginError);
    }
    let port = port.filter(|port| !port.is_empty());
    if let Some(port) = port
        && (!port.bytes().all(|c| c.is_ascii_digit()) || port.parse::<u16>().is_err())
    {
        return Err(OriginError);
    }
    Ok(Some(Origin {
        scheme,
        host: host.to_owned(),
        port: port.map(str::to_owned),
    }))
}

pub fn canonical_origin(raw: &str) -> Result<String, OriginError> {
    let origin = target_origin(raw)?.ok_or(OriginError)?;
    if origin.port_number() == 0 || origin.host.contains(['*', ';']) {
        return Err(OriginError);
    }
    Ok(origin.key())
}

fn ascii_name(host: &str) -> bool {
    let name = host.strip_suffix('.').unwrap_or(host);
    let last = name.rsplit('.').next().unwrap_or_default();
    let numeric = last.bytes().all(|c| c.is_ascii_digit())
        || last
            .strip_prefix("0x")
            .or_else(|| last.strip_prefix("0X"))
            .is_some_and(|hex| hex.bytes().all(|c| c.is_ascii_hexdigit()));
    name.split('.').all(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    }) && (!numeric || host.parse::<std::net::Ipv4Addr>().is_ok())
}

pub fn split_url(raw: &str) -> Result<(Origin, &str), OriginError> {
    let start = raw.find("://").ok_or(OriginError)? + 3;
    let end = raw[start..]
        .find(['/', '?'])
        .map_or(raw.len(), |index| start + index);
    let origin = target_origin(&raw[..end])?.ok_or(OriginError)?;
    let rest = &raw[end..];
    if rest.len() > 2048
        || rest
            .bytes()
            .any(|c| c <= b' ' || c >= 127 || matches!(c, b'#' | b'\\'))
    {
        return Err(OriginError);
    }
    Ok((origin, rest))
}

pub fn key(raw: &str) -> String {
    match target_origin(raw) {
        Ok(Some(origin)) => origin.key(),
        _ => raw.to_owned(),
    }
}

pub fn browser_connect_source_supported(raw: &str) -> bool {
    !raw.contains("://[")
}

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
        normalized.host = normalized.host.to_lowercase();
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
    // Validate with the standard URL implementation, but do not adopt WHATWG
    // normalization for saved catalog identities (e.g. :0443 or expanded IPv6).
    let parsed = url::Url::parse(raw).map_err(|_| OriginError)?;
    if parsed.host_str().is_none() {
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
        // Percent-encoded names have a different interpretation across URL
        // implementations and are not safe authentication audience identities.
        if host.contains(['%', '[', ']']) {
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
    if origin.port.as_deref() == Some("0") || origin.host.contains(['*', ';']) {
        return Err(OriginError);
    }
    Ok(origin.key())
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

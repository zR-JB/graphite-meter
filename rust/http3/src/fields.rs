//! HTTP messages as field sections (RFC 9114 §4.2-4.3, RFC 9220): validation, size and conversion.
use crate::qpack::{self, Invalid};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, Version, header, uri};

/// A decoded message head and the field section size (RFC 9114 §4.2.2) it was checked against.
pub(crate) struct Head<T> {
    pub(crate) message: T,
    pub(crate) content_length: Option<u64>,
    pub(crate) size: u64,
}

#[derive(Default)]
struct Fields {
    headers: HeaderMap,
    content_length: Option<u64>,
    size: u64,
    regular: bool,
}

impl Fields {
    /// Counts a field against the limit before any of it is kept.
    fn count(&mut self, name: &[u8], value: &[u8], limit: u64) -> Result<(), Invalid> {
        self.size += (name.len() + value.len() + 32) as u64;
        if self.size > limit { Err(Invalid::TooLarge) } else { Ok(()) }
    }

    /// Pseudo-header fields precede all others.
    fn pseudo(&self) -> Result<(), Invalid> {
        if self.regular { Err(Invalid::Malformed) } else { Ok(()) }
    }

    fn regular(&mut self, name: &[u8], value: &[u8]) -> Result<(), Invalid> {
        let (name, value) = regular(name, value)?;
        self.regular = true;
        if name == header::CONTENT_LENGTH {
            let length = std::str::from_utf8(value.as_bytes())
                .ok()
                .filter(|digits| !digits.is_empty() && digits.bytes().all(|digit| digit.is_ascii_digit()))
                .and_then(|digits| digits.parse().ok())
                .ok_or(Invalid::Malformed)?;
            if *self.content_length.get_or_insert(length) != length {
                return Err(Invalid::Malformed);
            }
        }
        self.headers.append(name, value);
        Ok(())
    }

    fn head<T>(self, mut message: T, headers: impl FnOnce(&mut T) -> &mut HeaderMap) -> Head<T> {
        *headers(&mut message) = self.headers;
        Head {
            message,
            content_length: self.content_length,
            size: self.size,
        }
    }
}

/// A regular field: lowercase, valid, and not specific to an HTTP/1.1 connection.
fn regular(name: &[u8], value: &[u8]) -> Result<(HeaderName, HeaderValue), Invalid> {
    let name = HeaderName::from_lowercase(name).map_err(|_| Invalid::Malformed)?;
    if connection_specific(&name) || name == header::TE && value != b"trailers" {
        return Err(Invalid::Malformed);
    }
    Ok((name, HeaderValue::from_bytes(value).map_err(|_| Invalid::Malformed)?))
}

fn connection_specific(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection" | "keep-alive" | "proxy-connection" | "transfer-encoding" | "upgrade"
    )
}

/// Sets a pseudo-header once; a repeat or an invalid value is malformed.
fn set<T, E>(slot: &mut Option<T>, value: Result<T, E>) -> Result<(), Invalid> {
    match (slot.is_some(), value) {
        (false, Ok(value)) => {
            *slot = Some(value);
            Ok(())
        }
        _ => Err(Invalid::Malformed),
    }
}

/// A request our routes serve. Plain CONNECT and other extended CONNECT protocols are `Unsupported`.
pub(crate) fn decode_request(section: &[u8], limit: u64) -> Result<Head<http::Request<()>>, Invalid> {
    let mut fields = Fields::default();
    let (mut method, mut scheme, mut authority, mut path, mut protocol) = (None, None, None, None, None);
    qpack::decode(section, |name, value| {
        fields.count(name, value, limit)?;
        match name {
            b":method" => set(&mut method, Method::from_bytes(value)),
            b":scheme" => set(&mut scheme, uri::Scheme::try_from(value)),
            b":authority" => set(&mut authority, uri::Authority::try_from(value)),
            b":path" => set(&mut path, uri::PathAndQuery::try_from(value)),
            b":protocol" => set(&mut protocol, Ok::<_, ()>(matches!(value, b"webtransport" | b"webtransport-h3"))),
            _ if name.starts_with(b":") => Err(Invalid::Malformed),
            _ => return fields.regular(name, value),
        }?;
        fields.pseudo()
    })?;
    let method = method.ok_or(Invalid::Malformed)?;
    if method == Method::CONNECT {
        let extended = scheme.is_some() && path.is_some() && authority.is_some();
        match protocol {
            Some(true) if extended => {}
            Some(_) if extended => return Err(Invalid::Unsupported),
            None if authority.is_some() && scheme.is_none() && path.is_none() => return Err(Invalid::Unsupported),
            _ => return Err(Invalid::Malformed),
        }
    } else if protocol.is_some() {
        return Err(Invalid::Malformed);
    }
    let authority = request_authority(authority, &fields.headers)?;
    let path = path.ok_or(Invalid::Malformed)?;
    if !path.as_str().starts_with('/') && !(method == Method::OPTIONS && path == "*") {
        return Err(Invalid::Malformed);
    }
    let mut parts = uri::Parts::default();
    (parts.scheme, parts.authority, parts.path_and_query) =
        (Some(scheme.ok_or(Invalid::Malformed)?), Some(authority), Some(path));
    let mut request = http::Request::new(());
    *request.method_mut() = method;
    *request.uri_mut() = Uri::from_parts(parts).map_err(|_| Invalid::Malformed)?;
    *request.version_mut() = Version::HTTP_3;
    Ok(fields.head(request, http::Request::headers_mut))
}

/// Host may stand in for :authority and must agree with it (RFC 9114 §4.3.1); neither carries userinfo.
fn request_authority(authority: Option<uri::Authority>, headers: &HeaderMap) -> Result<uri::Authority, Invalid> {
    let mut hosts = headers.get_all(header::HOST).iter();
    let authority = match (authority, hosts.next(), hosts.next()) {
        (_, Some(_), Some(_)) | (None, None, _) => return Err(Invalid::Malformed),
        (Some(authority), Some(host), _) if authority.as_str().as_bytes() != host.as_bytes() => {
            return Err(Invalid::Malformed);
        }
        (Some(authority), _, _) => authority,
        (None, Some(host), _) => uri::Authority::try_from(host.as_bytes()).map_err(|_| Invalid::Malformed)?,
    };
    if authority.as_str().contains('@') {
        return Err(Invalid::Malformed);
    }
    Ok(authority)
}

pub(crate) fn decode_response(section: &[u8], limit: u64) -> Result<Head<http::Response<()>>, Invalid> {
    let mut fields = Fields::default();
    let mut status = None;
    qpack::decode(section, |name, value| {
        fields.count(name, value, limit)?;
        match name {
            // HTTP/3 has no 101 (RFC 9114 §4.5), so a head that claims it is malformed.
            b":status" if value != b"101" => set(&mut status, StatusCode::from_bytes(value))?,
            _ if name.starts_with(b":") => return Err(Invalid::Malformed),
            _ => return fields.regular(name, value),
        }
        fields.pseudo()
    })?;
    let mut response = http::Response::new(());
    *response.status_mut() = status.ok_or(Invalid::Malformed)?;
    *response.version_mut() = Version::HTTP_3;
    Ok(fields.head(response, http::Response::headers_mut))
}

/// Trailers are validated and dropped: no route reads them.
pub(crate) fn check_trailers(section: &[u8], limit: u64) -> Result<(), Invalid> {
    let mut fields = Fields::default();
    qpack::decode(section, |name, value| {
        fields.count(name, value, limit)?;
        regular(name, value).map(drop)
    })
}

/// Encodes a request head; `protocol` makes it an extended CONNECT.
pub(crate) fn encode_request(
    request: &http::request::Parts,
    protocol: Option<&str>,
    limit: Option<u64>,
) -> Result<Vec<u8>, Invalid> {
    let uri = &request.uri;
    let pseudo = [
        (":method", request.method.as_str()),
        (":scheme", uri.scheme_str().ok_or(Invalid::Malformed)?),
        (":authority", uri.authority().ok_or(Invalid::Malformed)?.as_str()),
        (":path", uri.path_and_query().map_or("/", uri::PathAndQuery::as_str)),
        (":protocol", protocol.unwrap_or_default()),
    ];
    encode(&pseudo[..if protocol.is_some() { 5 } else { 4 }], &request.headers, limit)
}

pub(crate) fn encode_response(response: &http::response::Parts, limit: Option<u64>) -> Result<Vec<u8>, Invalid> {
    encode(&[(":status", response.status.as_str())], &response.headers, limit)
}

fn encode(pseudo: &[(&str, &str)], headers: &HeaderMap, limit: Option<u64>) -> Result<Vec<u8>, Invalid> {
    let fields = || {
        let regular = headers.iter().filter(|(name, _)| !connection_specific(name));
        pseudo
            .iter()
            .map(|&(name, value)| (name.as_bytes(), value.as_bytes()))
            .chain(regular.map(|(name, value)| (name.as_str().as_bytes(), value.as_bytes())))
    };
    let size = fields()
        .map(|(name, value)| (name.len() + value.len() + 32) as u64)
        .sum::<u64>();
    if limit.is_some_and(|limit| size > limit) {
        return Err(Invalid::TooLarge);
    }
    let mut section = Vec::with_capacity(size as usize / 2);
    qpack::encode(fields(), &mut section);
    Ok(section)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::section;

    const GET: [(&str, &str); 4] =
        [(":method", "GET"), (":scheme", "https"), (":authority", "meter.example"), (":path", "/")];

    fn request(fields: &[(&str, &str)]) -> Result<http::Request<()>, Invalid> {
        decode_request(&section(fields), 4096).map(|head| head.message)
    }

    fn with(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
        [&GET[..], extra].concat()
    }

    fn connect(fields: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
        [&[(":method", "CONNECT")][..], fields].concat()
    }

    #[test]
    fn requests_become_absolute_http3_requests() {
        let head = decode_request(&section(&with(&[("content-length", "7"), ("content-length", "7")])), 4096).unwrap();
        assert_eq!(head.message.uri(), "https://meter.example/");
        assert_eq!((head.message.version(), head.content_length), (Version::HTTP_3, Some(7)));
        let fields = GET.iter().chain(&[("content-length", "7"); 2]);
        assert_eq!(head.size, fields.map(|(n, v)| n.len() + v.len() + 32).sum::<usize>() as u64);
        for protocol in ["webtransport", "webtransport-h3"] {
            let session = connect(&[(":protocol", protocol), GET[1], GET[2], (":path", "/wt")]);
            assert_eq!(request(&session).unwrap().method(), Method::CONNECT);
        }
        let host = [GET[0], GET[1], (":path", "/p"), ("host", "meter.example")];
        assert_eq!(request(&host).unwrap().uri(), "https://meter.example/p");
        assert!(request(&with(&[("host", "meter.example"), ("te", "trailers")])).is_ok());
        assert!(request(&[(":method", "OPTIONS"), GET[1], GET[2], (":path", "*")]).is_ok());
    }

    #[test]
    fn malformed_and_unsupported_requests() {
        let malformed = [
            with(&[("Upper", "x")]),
            [&[("x", "1")][..], &GET].concat(),
            with(&[(":path", "/again")]),
            with(&[("connection", "close")]),
            with(&[("content-length", "7"), ("content-length", "8")]),
            with(&[("host", "other.example")]),
            with(&[(":protocol", "webtransport")]),
            GET[1..].to_vec(),
            vec![GET[0], GET[1], GET[2], (":path", "relative")],
            vec![GET[0], GET[1], (":authority", "user@meter.example"), GET[3]],
            connect(&[(":protocol", "webtransport"), GET[1], GET[2]]),
        ];
        for fields in malformed {
            assert_eq!(request(&fields).err(), Some(Invalid::Malformed), "{fields:?}");
        }
        for fields in [
            connect(&[(":authority", "meter.example:443")]),
            connect(&[(":protocol", "websocket"), GET[1], GET[2], GET[3]]),
        ] {
            assert_eq!(request(&fields).err(), Some(Invalid::Unsupported), "{fields:?}");
        }
    }

    #[test]
    fn the_size_limit_is_exact_and_qpack_errors_are_their_own() {
        let exact = GET
            .iter()
            .map(|(name, value)| name.len() + value.len() + 32)
            .sum::<usize>() as u64;
        assert!(decode_request(&section(&GET), exact).is_ok());
        assert_eq!(decode_request(&section(&GET), exact - 1).err(), Some(Invalid::TooLarge));
        assert_eq!(decode_request(&[0x01, 0x00], 4096).err(), Some(Invalid::Qpack));
    }

    #[test]
    fn responses_and_trailers() {
        let response = decode_response(&section(&[(":status", "204"), ("x", "y")]), 4096)
            .unwrap()
            .message;
        assert_eq!(
            (response.status(), response.headers()["x"].as_bytes()),
            (StatusCode::NO_CONTENT, &b"y"[..])
        );
        for fields in [&[("x", "y")][..], &[(":status", "2000")], &[(":status", "200"), (":path", "/")]] {
            assert_eq!(decode_response(&section(fields), 4096).err(), Some(Invalid::Malformed), "{fields:?}");
        }
        assert_eq!(check_trailers(&section(&[("x-checksum", "1")]), 4096), Ok(()));
        assert_eq!(check_trailers(&section(&[(":status", "200")]), 4096), Err(Invalid::Malformed));
        assert_eq!(check_trailers(&section(&[("x", "y")]), 33), Err(Invalid::TooLarge));
    }

    #[test]
    fn heads_encode_within_the_peer_limit() {
        let (parts, ()) = http::Request::connect("https://meter.example/wt/ping")
            .header(header::CONNECTION, "close")
            .header("authorization", "Bearer token")
            .body(())
            .unwrap()
            .into_parts();
        let encoded = encode_request(&parts, Some("webtransport-h3"), Some(4096)).unwrap();
        let decoded = decode_request(&encoded, 4096).unwrap().message;
        assert_eq!((decoded.uri(), decoded.headers().len()), (&parts.uri, 1), "without connection fields");
        assert_eq!(encode_request(&parts, None, Some(100)), Err(Invalid::TooLarge));
        let (parts, ()) = http::Response::builder().status(431).body(()).unwrap().into_parts();
        let encoded = encode_response(&parts, None).unwrap();
        assert_eq!(decode_response(&encoded, 4096).unwrap().message.status(), 431);
    }
}

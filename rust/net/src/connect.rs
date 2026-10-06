//! Connections to HTTP(S) targets: direct, through a CONNECT tunnel, in absolute form or through SOCKS5.
use crate::{
    Verify, client_config, dial,
    proxy::{Proxy, UnusableProxy, Upstream},
    socks,
};
use graphite_meter_proto::{
    discovery::Protocol,
    origin::{Host, Origin, Scheme},
};
use rustls::pki_types::ServerName;
use std::{fmt, io, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

/// The longest CONNECT response head a proxy may send.
const MAX_HEAD_BYTES: usize = 64 * 1024;
/// The longest a proxy may take past the TCP connect: Go's bound for CONNECT.
const PROXY_TIMEOUT: Duration = Duration::from_secs(60);

pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

/// An open connection to a target, past any proxy and through TLS for HTTPS.
pub struct Connection {
    pub stream: Box<dyn Stream>,
    /// The protocol the target's TLS handshake agreed on.
    pub alpn: Option<Vec<u8>>,
    pub form: RequestForm,
}

/// How requests on a connection name their target; it prints without credentials.
#[derive(Clone, PartialEq, Eq)]
pub enum RequestForm {
    /// `GET /path`, to the target itself.
    Origin,
    /// `GET http://host/path`, to an HTTP proxy, with its `Proxy-Authorization` value.
    Absolute { authorization: Option<String> },
}

impl fmt::Debug for RequestForm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Origin => formatter.write_str("Origin"),
            Self::Absolute { authorization } => formatter
                .debug_struct("Absolute")
                .field("authorization", &authorization.as_ref().map(|_| "<redacted>"))
                .finish(),
        }
    }
}

/// Why a connection could not be opened, decided where it failed.
#[derive(Debug)]
pub enum ConnectError {
    Proxy(UnusableProxy),
    /// The target or proxy resolved or connected nowhere in the dial timeout, or the proxy handshake overran its bound.
    Unreachable(io::Error),
    /// The proxy refused the connection or broke its protocol.
    Refused(String),
    /// The TLS handshake with the target or an HTTPS proxy failed.
    Tls(rustls::Error),
    Io(io::Error),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Proxy(unusable) => unusable.fmt(formatter),
            Self::Unreachable(error) | Self::Io(error) => error.fmt(formatter),
            Self::Refused(reason) => formatter.write_str(reason),
            Self::Tls(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<io::Error> for ConnectError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<UnusableProxy> for ConnectError {
    fn from(unusable: UnusableProxy) -> Self {
        Self::Proxy(unusable)
    }
}

/// Opens connections through the environment's proxies, verifying HTTPS targets and proxies as `verify` says.
pub struct Connector {
    proxy: Proxy,
    verify: Verify,
}

impl Connector {
    pub fn new(proxy: Proxy, verify: Verify) -> Self {
        Self { proxy, verify }
    }

    /// Connects to `target`, offering `alpn` in its TLS handshake when it is HTTPS.
    pub async fn connect(&self, target: &Origin, alpn: Option<Protocol>) -> Result<Connection, ConnectError> {
        let tls = target.scheme == Scheme::Https;
        let (stream, form) = match self.proxy.route(target)? {
            None => (Box::new(reach(&target.host, target.port).await?) as Box<dyn Stream>, RequestForm::Origin),
            Some(upstream) => {
                let (host, port) = upstream.address();
                let stream = reach(host, port).await?;
                let handshake = self.through(upstream, stream, target, tls);
                tokio::time::timeout(PROXY_TIMEOUT, handshake)
                    .await
                    .unwrap_or_else(|_| {
                        let silent = io::Error::new(io::ErrorKind::TimedOut, "proxy handshake timed out");
                        Err(ConnectError::Unreachable(silent))
                    })?
            }
        };
        if !tls {
            return Ok(Connection { stream, alpn: None, form });
        }
        let config = client_config(self.verify, alpn).await;
        let stream = secure(&TlsConnector::from(config), &target.host, stream).await?;
        let alpn = stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
        Ok(Connection { stream: Box::new(stream), alpn, form })
    }

    /// The proxy's part on `stream`: its TLS for an HTTPS proxy, then SOCKS5, a CONNECT tunnel or nothing.
    async fn through(
        &self,
        upstream: &Upstream,
        mut stream: TcpStream,
        target: &Origin,
        tls: bool,
    ) -> Result<(Box<dyn Stream>, RequestForm), ConnectError> {
        let (origin, authorization) = match upstream {
            Upstream::Socks { login, .. } => {
                socks::connect(&mut stream, login.as_ref(), &target.host, target.port).await?;
                return Ok((Box::new(stream), RequestForm::Origin));
            }
            Upstream::Http { origin, authorization } => (origin, authorization),
        };
        let stream: Box<dyn Stream> = match origin.scheme {
            Scheme::Https => {
                let hop = TlsConnector::from(client_config(self.verify, None).await);
                Box::new(secure(&hop, &origin.host, stream).await?)
            }
            Scheme::Http => Box::new(stream),
        };
        if tls {
            Ok((tunnel(stream, target, authorization.as_deref()).await?, RequestForm::Origin))
        } else {
            Ok((stream, RequestForm::Absolute { authorization: authorization.clone() }))
        }
    }
}

async fn reach(host: &Host, port: u16) -> Result<TcpStream, ConnectError> {
    dial::dial(host, port).await.map_err(ConnectError::Unreachable)
}

async fn secure<S: Stream>(
    tls: &TlsConnector,
    host: &Host,
    stream: S,
) -> Result<tokio_rustls::client::TlsStream<S>, ConnectError> {
    let name = match host {
        Host::Name(name) => ServerName::try_from(name.clone()).map_err(|error| io::Error::other(error.to_string()))?,
        Host::Ip(ip) => ServerName::IpAddress((*ip).into()),
    };
    tls.connect(name, stream).await.map_err(|error| {
        match error.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>()) {
            Some(tls) => ConnectError::Tls(tls.clone()),
            None => ConnectError::Io(error),
        }
    })
}

/// Asks an HTTP proxy for a tunnel to `target`, which only a 200 grants.
async fn tunnel(
    mut stream: Box<dyn Stream>,
    target: &Origin,
    authorization: Option<&str>,
) -> Result<Box<dyn Stream>, ConnectError> {
    let authority = format!("{}:{}", target.host, target.port);
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(authorization) = authorization {
        request.push_str(&format!("Proxy-Authorization: {authorization}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    let head = read_head(&mut stream).await?;
    let status = head.lines().next().unwrap_or_default();
    let (version, rest) = status.split_once(' ').unwrap_or_default();
    let code = rest.split(' ').next().unwrap_or_default();
    if !version.starts_with("HTTP/") || code.len() != 3 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ConnectError::Refused("proxy sent a malformed CONNECT response".into()));
    }
    if code != "200" {
        return Err(ConnectError::Refused(format!("proxy refused CONNECT with {rest}")));
    }
    Ok(stream)
}

/// A response head ending in an empty line, CRs optional, read byte by byte so the tunnel's first bytes stay unread.
async fn read_head(stream: &mut Box<dyn Stream>) -> Result<String, ConnectError> {
    let mut head = Vec::new();
    while !(head.ends_with(b"\n\n") || head.ends_with(b"\n\r\n")) {
        if head.len() == MAX_HEAD_BYTES {
            return Err(ConnectError::Refused("proxy sent an oversized CONNECT response".into()));
        }
        head.push(stream.read_u8().await?);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

//! Connections to HTTP(S) targets: direct, through a CONNECT tunnel, in absolute form or through SOCKS5.
use crate::{
    Verify, client_config, dial,
    proxy::{Proxy, UnusableProxy, Upstream},
    socks,
};
use graphite_meter_proto::origin::{Host, Origin, Scheme};
use rustls::{ClientConfig, pki_types::ServerName};
use std::{fmt, io, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::OnceCell,
};
use tokio_rustls::TlsConnector;

/// The longest CONNECT response head a proxy may send.
const MAX_HEAD_BYTES: usize = 64 * 1024;

pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

/// An open connection to a target, past any proxy and through TLS for HTTPS.
pub struct Connection {
    pub stream: Box<dyn Stream>,
    /// The protocol the target's TLS handshake agreed on.
    pub alpn: Option<Vec<u8>>,
    pub form: RequestForm,
}

/// How requests on a connection name their target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestForm {
    /// `GET /path`, to the target itself.
    Origin,
    /// `GET http://host/path`, to an HTTP proxy, with its `Proxy-Authorization` value.
    Absolute { authorization: Option<String> },
}

/// Why a connection could not be opened, decided where it failed.
#[derive(Debug)]
pub enum ConnectError {
    Proxy(UnusableProxy),
    /// The target or its proxy resolved to nothing or accepted no connection within the dial timeout.
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

/// Opens connections through the environment's proxies, verifying an HTTPS proxy as `verify` says.
pub struct Connector {
    proxy: Proxy,
    verify: Verify,
    /// TLS to an HTTPS proxy, built on first use; it offers no ALPN, so the proxy answers in HTTP/1.1.
    hop: OnceCell<TlsConnector>,
}

impl Connector {
    pub fn new(proxy: Proxy, verify: Verify) -> Self {
        Self { proxy, verify, hop: OnceCell::new() }
    }

    /// Connects to `target`, through TLS configured by `tls` exactly when it is HTTPS.
    pub async fn connect(&self, target: &Origin, tls: Option<&Arc<ClientConfig>>) -> Result<Connection, ConnectError> {
        if (target.scheme == Scheme::Https) != tls.is_some() {
            let mismatch = "TLS configuration does not match the target scheme";
            return Err(io::Error::new(io::ErrorKind::InvalidInput, mismatch).into());
        }
        let (stream, form): (Box<dyn Stream>, _) = match self.proxy.route(target)? {
            None => (Box::new(reach(&target.host, target.port).await?), RequestForm::Origin),
            Some(Upstream::Socks { host, port, login }) => {
                let mut stream = reach(host, *port).await?;
                socks::connect(&mut stream, login.as_ref(), &target.host, target.port).await?;
                (Box::new(stream), RequestForm::Origin)
            }
            Some(Upstream::Http { origin, authorization }) => {
                let stream = reach(&origin.host, origin.port).await?;
                let stream: Box<dyn Stream> = match origin.scheme {
                    Scheme::Https => Box::new(secure(self.hop().await, &origin.host, stream).await?),
                    Scheme::Http => Box::new(stream),
                };
                match tls {
                    Some(_) => (tunnel(stream, target, authorization.as_deref()).await?, RequestForm::Origin),
                    None => (stream, RequestForm::Absolute { authorization: authorization.clone() }),
                }
            }
        };
        let Some(tls) = tls else {
            return Ok(Connection { stream, alpn: None, form });
        };
        let stream = secure(&TlsConnector::from(tls.clone()), &target.host, stream).await?;
        let alpn = stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
        Ok(Connection { stream: Box::new(stream), alpn, form })
    }

    async fn hop(&self) -> &TlsConnector {
        let build = || async {
            let config = client_config(self.verify, rustls::DEFAULT_VERSIONS, &[]).await;
            TlsConnector::from(Arc::new(config))
        };
        self.hop.get_or_init(build).await
    }
}

async fn reach(host: &Host, port: u16) -> Result<tokio::net::TcpStream, ConnectError> {
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

/// A response head, read byte by byte so the tunnel's first bytes stay unread.
async fn read_head(stream: &mut Box<dyn Stream>) -> Result<String, ConnectError> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() == MAX_HEAD_BYTES {
            return Err(ConnectError::Refused("proxy sent an oversized CONNECT response".into()));
        }
        head.push(stream.read_u8().await?);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

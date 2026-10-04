use base64::{Engine, engine::general_purpose::STANDARD};
use graphite_meter_core::origin::{Origin, ascii_host, target_origin};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use std::{
    io,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    task::JoinSet,
};
use tokio_rustls::TlsConnector;

pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

pub struct Connection {
    pub stream: Box<dyn Stream>,
    pub alpn: Option<Vec<u8>>,
    pub absolute_form: bool,
    pub proxy_authorization: Option<http::HeaderValue>,
}

/// Connects to `target` over `tls` for HTTPS, through its proxy if one applies. An HTTPS proxy's
/// hop takes the connector `hop` yields, awaited only then: Go's transport verifies the proxy as it
/// does the target, -insecure included, for cleartext and HTTPS targets alike.
pub async fn connect<H, E>(proxy: &Proxy, target: &Origin, tls: Option<&TlsConnector>, hop: H) -> io::Result<Connection>
where
    H: Future<Output = Result<TlsConnector, E>>,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if !matches!((target.scheme.as_str(), tls), ("https", Some(_)) | ("http", None)) {
        return Err(io::Error::other("TLS configuration does not match the target scheme"));
    }
    let port = target.port_number();
    let mut absolute_form = false;
    let mut proxy_authorization = None;
    let stream: Box<dyn Stream> = match proxy.route(target) {
        None => Box::new(tcp(&target.host, port).await?),
        Some(Err(unusable)) => return Err(io::Error::new(io::ErrorKind::InvalidInput, unusable.clone())),
        Some(Ok(Upstream { origin, socks: Some(socks), .. })) => {
            let mut tcp = tcp(&origin.host, origin.port_number()).await?;
            socks.connect(&mut tcp, &target.host, port).await?;
            Box::new(tcp)
        }
        Some(Ok(upstream)) => {
            let tcp = tcp(&upstream.origin.host, upstream.origin.port_number()).await?;
            let stream: Box<dyn Stream> = if upstream.origin.scheme == "https" {
                let hop = hop.await.map_err(io::Error::other)?;
                Box::new(hop.connect(server_name(&upstream.origin.host)?, tcp).await?)
            } else {
                Box::new(tcp)
            };
            if target.scheme == "https" {
                let authority = match target.port {
                    Some(_) => target.authority(),
                    None => format!("{}:{port}", target.authority()),
                };
                tunnel(stream, &authority, upstream.authorization.as_deref()).await?
            } else {
                absolute_form = true;
                proxy_authorization = upstream
                    .authorization
                    .as_ref()
                    .map(|value| {
                        let mut header = http::HeaderValue::from_str(value).map_err(io::Error::other)?;
                        header.set_sensitive(true);
                        Ok::<_, io::Error>(header)
                    })
                    .transpose()?;
                stream
            }
        }
    };
    let (stream, alpn): (Box<dyn Stream>, _) = match tls {
        Some(tls) => {
            let stream = tls.connect(server_name(&target.host)?, stream).await?;
            let alpn = stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
            (Box::new(stream), alpn)
        }
        None => (stream, None),
    };
    Ok(Connection { stream, alpn, absolute_form, proxy_authorization })
}

fn server_name(host: &str) -> io::Result<ServerName<'static>> {
    ServerName::try_from(host.to_owned()).map_err(io::Error::other)
}

#[derive(Debug)]
pub struct Unreachable(io::Error);
impl std::fmt::Display for Unreachable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}
impl std::error::Error for Unreachable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

fn unreachable(error: io::Error) -> io::Error {
    io::Error::new(error.kind(), Unreachable(error))
}

pub async fn resolve(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    Ok(tokio::net::lookup_host((host, port))
        .await
        .map_err(unreachable)?
        .collect())
}

/// Go's dialer gives up after 10 s; a second less keeps the client's 10 s control deadline from ending the dial.
async fn tcp(host: &str, port: u16) -> io::Result<TcpStream> {
    tokio::time::timeout(Duration::from_secs(9), dial(host, port))
        .await
        .unwrap_or_else(|_| Err(unreachable(io::ErrorKind::TimedOut.into())))
}

async fn dial(host: &str, port: u16) -> io::Result<TcpStream> {
    let addresses = resolve(host, port).await?;
    let (v6, v4): (Vec<_>, Vec<_>) = addresses.into_iter().partition(SocketAddr::is_ipv6);
    let mut ordered = Vec::with_capacity(v6.len() + v4.len());
    let (mut v6, mut v4) = (v6.into_iter(), v4.into_iter());
    loop {
        match (v6.next(), v4.next()) {
            (None, None) => break,
            (a, b) => ordered.extend(a.into_iter().chain(b)),
        }
    }
    let mut attempts = JoinSet::new();
    let mut pending = ordered.into_iter();
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address resolved");
    loop {
        if let Some(address) = pending.next() {
            attempts.spawn(TcpStream::connect(address));
        } else if attempts.is_empty() {
            return Err(unreachable(last));
        }
        let stagger = tokio::time::sleep(Duration::from_millis(250));
        tokio::select! {
            Some(result) = attempts.join_next() => match result.map_err(io::Error::other)? {
                Ok(stream) => {
                    stream.set_nodelay(true)?;
                    keep_alive(&stream);
                    return Ok(stream);
                }
                Err(error) => last = error,
            },
            () = stagger, if pending.len() > 0 => {}
        }
    }
}

/// Go's dialer probes an idle peer after 30 s, then every 30 s; like Go, a socket that refuses
/// the options still connects.
fn keep_alive(stream: &TcpStream) {
    const PERIOD: Duration = Duration::from_secs(30);
    let probes = socket2::TcpKeepalive::new().with_time(PERIOD);
    #[cfg(any(
        target_os = "android",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "fuchsia",
        target_os = "illumos",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "netbsd",
        target_os = "windows",
    ))]
    let probes = probes.with_interval(PERIOD);
    let _ = socket2::SockRef::from(stream).set_tcp_keepalive(&probes);
}

async fn tunnel(stream: Box<dyn Stream>, authority: &str, authorization: Option<&str>) -> io::Result<Box<dyn Stream>> {
    let (mut sender, connection) = hyper::client::conn::http1::handshake::<_, String>(TokioIo::new(stream))
        .await
        .map_err(io::Error::other)?;
    let mut request = http::Request::connect(authority).header(http::header::HOST, authority);
    if let Some(authorization) = authorization {
        request = request.header(http::header::PROXY_AUTHORIZATION, authorization);
    }
    let request = request.body(String::new()).map_err(io::Error::other)?;
    tokio::spawn(connection.with_upgrades());
    let response = sender.send_request(request).await.map_err(io::Error::other)?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        return Err(io::Error::other(format!("proxy refused CONNECT with HTTP {status}")));
    }
    let upgraded = hyper::upgrade::on(response).await.map_err(io::Error::other)?;
    Ok(Box::new(TokioIo::new(upgraded)))
}

#[derive(Clone, Default)]
pub struct Proxy {
    http: Option<Result<Upstream, UnusableProxy>>,
    https: Option<Result<Upstream, UnusableProxy>>,
    /// NO_PROXY's entries, each with its port as written: Go compares it with the target's as a string.
    bypass: Vec<(Bypass, Option<String>)>,
    /// Running under CGI, where HTTP_PROXY fails every cleartext request.
    cgi: bool,
}

/// A proxy variable this client cannot use; each request it would carry fails with it.
#[derive(Clone, Debug)]
pub struct UnusableProxy {
    variable: &'static str,
    reason: &'static str,
}
impl std::fmt::Display for UnusableProxy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} is not a usable proxy: {}", self.variable, self.reason)
    }
}
impl std::error::Error for UnusableProxy {}

#[derive(Clone)]
struct Upstream {
    origin: Origin,
    authorization: Option<String>,
    socks: Option<Socks>,
}

/// Go's SOCKS5 dialer, which net/http uses for socks5 and socks5h alike: it offers
/// username/password only when the proxy URL has user info, and passes host names to the proxy.
#[derive(Clone)]
struct Socks {
    user: Option<(Vec<u8>, Vec<u8>)>,
}

impl Socks {
    async fn connect(&self, stream: &mut TcpStream, host: &str, port: u16) -> io::Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let failed = |reason: String| io::Error::other(format!("socks connect: {reason}"));
        let greeting: &[u8] = if self.user.is_some() { &[5, 2, 0, 2] } else { &[5, 1, 0] };
        stream.write_all(greeting).await?;
        let mut reply = [0; 2];
        stream.read_exact(&mut reply).await?;
        if reply[0] != 5 {
            return Err(failed(format!("unexpected protocol version {}", reply[0])));
        }
        match (reply[1], &self.user) {
            (0, _) => {}
            (0xff, _) => return Err(failed("no acceptable authentication methods".into())),
            (2, Some((user, password))) => {
                if user.is_empty() || user.len() > 255 || password.len() > 255 {
                    return Err(failed("invalid username/password".into()));
                }
                let mut login = vec![1, user.len() as u8];
                login.extend_from_slice(user);
                login.push(password.len() as u8);
                login.extend_from_slice(password);
                stream.write_all(&login).await?;
                stream.read_exact(&mut reply).await?;
                if reply[0] != 1 {
                    return Err(failed("invalid username/password version".into()));
                }
                if reply[1] != 0 {
                    return Err(failed("username/password authentication failed".into()));
                }
            }
            (method, _) => return Err(failed(format!("unsupported authentication method {method}"))),
        }
        let mut request = vec![5, 1, 0];
        // Like Go's To4, an IPv4-mapped address goes as IPv4.
        match host.parse::<IpAddr>().map(|ip| ip.to_canonical()) {
            Ok(IpAddr::V4(ip)) => {
                request.push(1);
                request.extend_from_slice(&ip.octets());
            }
            Ok(IpAddr::V6(ip)) => {
                request.push(4);
                request.extend_from_slice(&ip.octets());
            }
            Err(_) => {
                let length = u8::try_from(host.len()).map_err(|_| failed("FQDN too long".into()))?;
                request.extend_from_slice(&[3, length]);
                request.extend_from_slice(host.as_bytes());
            }
        }
        request.extend_from_slice(&port.to_be_bytes());
        stream.write_all(&request).await?;
        let mut head = [0; 4];
        stream.read_exact(&mut head).await?;
        if head[0] != 5 {
            return Err(failed(format!("unexpected protocol version {}", head[0])));
        }
        if head[1] != 0 {
            return Err(failed(format!("unknown error {}", socks_reply(head[1]))));
        }
        if head[2] != 0 {
            return Err(failed("non-zero reserved field".into()));
        }
        let bound = match head[3] {
            1 => 4,
            4 => 16,
            3 => usize::from(stream.read_u8().await?),
            other => return Err(failed(format!("unknown address type {other}"))),
        };
        stream.read_exact(&mut vec![0; bound + 2]).await?;
        Ok(())
    }
}

fn socks_reply(code: u8) -> String {
    match code {
        1 => "general SOCKS server failure".into(),
        2 => "connection not allowed by ruleset".into(),
        3 => "network unreachable".into(),
        4 => "host unreachable".into(),
        5 => "connection refused".into(),
        6 => "TTL expired".into(),
        7 => "command not supported".into(),
        8 => "address type not supported".into(),
        code => format!("unknown code: {code}"),
    }
}

#[derive(Clone)]
enum Bypass {
    All,
    Network(ipnet::IpNet),
    Address(IpAddr),
    Domain { suffix: String, apex: bool },
}

impl Proxy {
    pub fn from_env() -> Self {
        Self::from_variables(|name| std::env::var(name).ok())
    }

    /// Go's ProxyFromEnvironment: HTTP_PROXY, HTTPS_PROXY and NO_PROXY, each before its lowercase
    /// spelling, and never ALL_PROXY. Under CGI, where a request's Proxy header becomes
    /// HTTP_PROXY, every cleartext request refuses it as Go's do, before NO_PROXY or loopback
    /// apply; HTTPS_PROXY still applies.
    fn from_variables(variable: impl Fn(&str) -> Option<String>) -> Self {
        let read = |names: [&'static str; 2]| {
            names.into_iter().find_map(|name| {
                variable(name)
                    .filter(|value| !value.is_empty())
                    .map(|value| (name, value))
            })
        };
        let (http, https) = (read(["HTTP_PROXY", "http_proxy"]), read(["HTTPS_PROXY", "https_proxy"]));
        let no_proxy = read(["NO_PROXY", "no_proxy"]).map(|(_, value)| value);
        let mut proxy = Self::named(
            http.as_ref().map(|(name, value)| (*name, value.as_str())),
            https.as_ref().map(|(name, value)| (*name, value.as_str())),
            no_proxy.as_deref().unwrap_or_default(),
        );
        if let Some((name, _)) = http
            && variable("REQUEST_METHOD").is_some_and(|method| !method.is_empty())
        {
            proxy.http = Some(Err(UnusableProxy {
                variable: name,
                reason: "a CGI request's Proxy header can set it",
            }));
            proxy.cgi = true;
        }
        proxy
    }

    pub fn new(http: &str, https: &str, no_proxy: &str) -> Self {
        Self::named(Some(("HTTP_PROXY", http)), Some(("HTTPS_PROXY", https)), no_proxy)
    }

    fn named(http: Option<(&'static str, &str)>, https: Option<(&'static str, &str)>, no_proxy: &str) -> Self {
        let upstream = |named: Option<(&'static str, &str)>| {
            let (variable, raw) = named.filter(|(_, raw)| !raw.trim().is_empty())?;
            Some(upstream(raw.trim()).map_err(|reason| UnusableProxy { variable, reason }))
        };
        Self {
            http: upstream(http),
            https: upstream(https),
            bypass: no_proxy.split(',').filter_map(bypass).collect(),
            cgi: false,
        }
    }

    fn route(&self, target: &Origin) -> Option<&Result<Upstream, UnusableProxy>> {
        if target.scheme == "https" {
            return self.https.as_ref().filter(|_| !self.bypassed(target));
        }
        // Go refuses a CGI request's HTTP_PROXY before it looks at NO_PROXY or loopback.
        self.http.as_ref().filter(|_| self.cgi || !self.bypassed(target))
    }

    fn bypassed(&self, target: &Origin) -> bool {
        let host = target.host.to_ascii_lowercase();
        // The target's port as written, else its scheme's default, as Go's canonicalAddr has it.
        let port = target.port.clone().unwrap_or_else(|| target.port_number().to_string());
        // Go's net.IP matches an IPv4-mapped address as IPv4.
        let ip = host.parse::<IpAddr>().ok().map(|ip| ip.to_canonical());
        host == "localhost"
            || ip.is_some_and(|ip| ip.is_loopback())
            || self.bypass.iter().any(|(rule, only)| {
                only.as_ref().is_none_or(|only| *only == port)
                    && match (rule, ip) {
                        (Bypass::All, _) => true,
                        (Bypass::Network(network), Some(ip)) => network.contains(&ip),
                        (Bypass::Address(address), Some(ip)) => *address == ip,
                        (Bypass::Domain { suffix, apex }, None) => {
                            host.ends_with(suffix.as_str()) || *apex && host == suffix[1..]
                        }
                        _ => false,
                    }
            })
    }
}

fn upstream(raw: &str) -> Result<Upstream, &'static str> {
    let raw = if raw.contains("://") { raw.to_owned() } else { format!("http://{raw}") };
    let (scheme, rest) = raw.split_once("://").ok_or("invalid proxy URL")?;
    let authority = rest.split('/').next().unwrap_or_default();
    let (credentials, host) = match authority.rsplit_once('@') {
        Some((credentials, host)) => (Some(credentials.split_once(':').unwrap_or((credentials, ""))), host),
        None => (None, authority),
    };
    let socks = matches!(scheme.to_ascii_lowercase().as_str(), "socks5" | "socks5h");
    if !socks && !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return Err("only HTTP, HTTPS and SOCKS5 proxies are supported");
    }
    let mut origin = target_origin(&format!("{}://{host}", if socks { "http" } else { scheme }))
        .ok()
        .flatten()
        .ok_or("invalid proxy URL")?;
    if socks {
        origin.scheme = "socks5".into();
        origin.port.get_or_insert_with(|| "1080".into());
        let decode = |part| percent_encoding::percent_decode_str(part).collect();
        let user = credentials.map(|(user, password)| (decode(user), decode(password)));
        return Ok(Upstream { origin, authorization: None, socks: Some(Socks { user }) });
    }
    let decode = |part| percent_encoding::percent_decode_str(part).decode_utf8_lossy();
    let authorization = credentials.map(|(user, password)| {
        let credentials = format!("{}:{}", decode(user), decode(password));
        format!("Basic {}", STANDARD.encode(credentials))
    });
    Ok(Upstream { origin, authorization, socks: None })
}

/// One NO_PROXY entry as Go reads it; one Go keeps as a host name that no target matches is left out.
fn bypass(entry: &str) -> Option<(Bypass, Option<String>)> {
    let entry = entry.trim().to_ascii_lowercase();
    if entry == "*" {
        return Some((Bypass::All, None));
    }
    // Go's ParseCIDR reads the address as IpAddr does, so one with a leading zero names no network.
    let address = entry.split_once('/').and_then(|(ip, _)| ip.parse::<IpAddr>().ok());
    if let Some(network) = address.and_then(|_| entry.parse().ok()) {
        return Some((Bypass::Network(unmapped(network)), None));
    }
    if let Ok(address) = entry.parse::<IpAddr>() {
        return Some((Bypass::Address(address.to_canonical()), None));
    }
    let (host, port) = match entry.strip_prefix('[') {
        // As Go's SplitHostPort has it, a bracketed host needs its port, which may be empty.
        Some(bracketed) => bracketed.split_once("]:")?,
        None => entry.split_once(':').unwrap_or((&entry, "")),
    };
    let port = (!port.is_empty()).then(|| port.to_owned());
    if let Ok(address) = host.parse::<IpAddr>() {
        return Some((Bypass::Address(address.to_canonical()), port));
    }
    // Go drops the star of a leading "*." alone, so "*example.com" names no host.
    let host = if host.starts_with("*.") { &host[1..] } else { host };
    let apex = !host.starts_with('.');
    let suffix = if apex { format!(".{host}") } else { host.to_owned() };
    // Go's idnaASCII: an international name matches the punycode target Go dials.
    let suffix = ascii_host(&suffix).filter(|_| !host.is_empty())?;
    Some((Bypass::Domain { suffix, apex }, port))
}

/// Go's net.IPNet holds an IPv4-mapped network as the IPv4 network it maps.
fn unmapped(network: ipnet::IpNet) -> ipnet::IpNet {
    let ipnet::IpNet::V6(v6) = network else {
        return network;
    };
    let mapped = v6.network().to_ipv4_mapped().zip(v6.prefix_len().checked_sub(96));
    mapped
        .and_then(|(v4, prefix)| ipnet::Ipv4Net::new(v4, prefix).ok())
        .map_or(network, ipnet::IpNet::V4)
}

mod pool;
pub mod trust;

pub use pool::Pool;

#[cfg(test)]
mod tests;

use base64::{Engine, engine::general_purpose::STANDARD};
use graphite_meter_core::origin::{Origin, target_origin};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use rustls_platform_verifier::BuilderVerifierExt;
use std::{
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
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

pub async fn connect(
    proxy: &Proxy,
    target: &Origin,
    tls: Option<&TlsConnector>,
) -> io::Result<Connection> {
    if (target.scheme == "https") != tls.is_some() {
        return Err(io::Error::other(
            "TLS configuration does not match the target scheme",
        ));
    }
    let port = target.port_number();
    let mut absolute_form = false;
    let mut proxy_authorization = None;
    let stream: Box<dyn Stream> = match proxy.route(target) {
        None => Box::new(tcp(&target.host, port).await?),
        Some(Err(unusable)) => return Err(io::Error::other(*unusable)),
        Some(Ok(upstream)) => {
            let tcp = tcp(&upstream.origin.host, upstream.origin.port_number()).await?;
            let stream: Box<dyn Stream> = if upstream.origin.scheme == "https" {
                let tls = proxy
                    .tls
                    .as_ref()
                    .ok_or_else(|| io::Error::other("proxy TLS is unavailable"))?;
                Box::new(
                    tls.connect(server_name(&upstream.origin.host)?, tcp)
                        .await?,
                )
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
                        let mut header =
                            http::HeaderValue::from_str(value).map_err(io::Error::other)?;
                        header.set_sensitive(true);
                        Ok::<_, io::Error>(header)
                    })
                    .transpose()?;
                stream
            }
        }
    };
    match (target.scheme.as_str(), tls) {
        ("https", Some(tls)) => {
            let stream = tls.connect(server_name(&target.host)?, stream).await?;
            let alpn = stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
            Ok(Connection {
                stream: Box::new(stream),
                alpn,
                absolute_form,
                proxy_authorization,
            })
        }
        ("http", None) => Ok(Connection {
            stream,
            alpn: None,
            absolute_form,
            proxy_authorization,
        }),
        _ => Err(io::Error::other(
            "TLS configuration does not match the target scheme",
        )),
    }
}

fn server_name(host: &str) -> io::Result<ServerName<'static>> {
    ServerName::try_from(host.to_owned()).map_err(io::Error::other)
}

async fn tcp(host: &str, port: u16) -> io::Result<TcpStream> {
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
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
            return Err(last);
        }
        let stagger = tokio::time::sleep(Duration::from_millis(250));
        tokio::select! {
            Some(result) = attempts.join_next() => match result.map_err(io::Error::other)? {
                Ok(stream) => {
                    stream.set_nodelay(true)?;
                    return Ok(stream);
                }
                Err(error) => last = error,
            },
            () = stagger, if pending.len() > 0 => {}
        }
    }
}

async fn tunnel(
    stream: Box<dyn Stream>,
    authority: &str,
    authorization: Option<&str>,
) -> io::Result<Box<dyn Stream>> {
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake::<_, String>(TokioIo::new(stream))
            .await
            .map_err(io::Error::other)?;
    let mut request = http::Request::connect(authority).header(http::header::HOST, authority);
    if let Some(authorization) = authorization {
        request = request.header(http::header::PROXY_AUTHORIZATION, authorization);
    }
    let request = request.body(String::new()).map_err(io::Error::other)?;
    tokio::spawn(connection.with_upgrades());
    let response = sender
        .send_request(request)
        .await
        .map_err(io::Error::other)?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        return Err(io::Error::other(format!(
            "proxy refused CONNECT with HTTP {status}"
        )));
    }
    let upgraded = hyper::upgrade::on(response)
        .await
        .map_err(io::Error::other)?;
    Ok(Box::new(TokioIo::new(upgraded)))
}

#[derive(Clone, Default)]
pub struct Proxy {
    http: Option<Result<Upstream, &'static str>>,
    https: Option<Result<Upstream, &'static str>>,
    bypass: Vec<Bypass>,
    tls: Option<TlsConnector>,
}

#[derive(Clone)]
struct Upstream {
    origin: Origin,
    authorization: Option<String>,
}

#[derive(Clone)]
enum Bypass {
    All,
    Network(ipnet::IpNet),
    Address(IpAddr, Option<u16>),
    Domain {
        suffix: String,
        apex: bool,
        port: Option<u16>,
    },
}

impl Proxy {
    pub fn from_env() -> Self {
        let read = |names: [&str; 2]| {
            names
                .iter()
                .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
                .unwrap_or_default()
        };
        if std::env::var_os("REQUEST_METHOD").is_some() {
            return Self::default();
        }
        let all = read(["ALL_PROXY", "all_proxy"]);
        let or_all = |value: String| if value.is_empty() { all.clone() } else { value };
        Self::new(
            &or_all(read(["HTTP_PROXY", "http_proxy"])),
            &or_all(read(["HTTPS_PROXY", "https_proxy"])),
            &read(["NO_PROXY", "no_proxy"]),
        )
    }

    pub fn new(http: &str, https: &str, no_proxy: &str) -> Self {
        let upstream = |raw: &str| (!raw.trim().is_empty()).then(|| upstream(raw.trim()));
        let (http, https) = (upstream(http), upstream(https));
        let tls = [&http, &https]
            .into_iter()
            .flatten()
            .flatten()
            .any(|upstream| upstream.origin.scheme == "https")
            .then(proxy_tls)
            .flatten();
        Self {
            http,
            https,
            bypass: no_proxy.split(',').filter_map(bypass).collect(),
            tls,
        }
    }

    fn route(&self, target: &Origin) -> Option<&Result<Upstream, &'static str>> {
        let upstream = if target.scheme == "https" {
            &self.https
        } else {
            &self.http
        };
        upstream.as_ref().filter(|_| !self.bypassed(target))
    }

    fn bypassed(&self, target: &Origin) -> bool {
        let host = target.host.to_ascii_lowercase();
        let port = target.port_number();
        let ip = host.parse::<IpAddr>().ok();
        host == "localhost"
            || ip.is_some_and(|ip| ip.is_loopback())
            || self.bypass.iter().any(|rule| match (rule, ip) {
                (Bypass::All, _) => true,
                (Bypass::Network(network), Some(ip)) => network.contains(&ip),
                (Bypass::Address(address, only), Some(ip)) => {
                    *address == ip && only.is_none_or(|only| only == port)
                }
                (
                    Bypass::Domain {
                        suffix,
                        apex,
                        port: only,
                    },
                    None,
                ) => {
                    (host.ends_with(suffix.as_str()) || *apex && host == suffix[1..])
                        && only.is_none_or(|only| only == port)
                }
                _ => false,
            })
    }
}

fn upstream(raw: &str) -> Result<Upstream, &'static str> {
    let raw = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };
    let (scheme, rest) = raw.split_once("://").ok_or("invalid proxy URL")?;
    let authority = rest.split('/').next().unwrap_or_default();
    let (credentials, host) = match authority.rsplit_once('@') {
        Some((credentials, host)) => (Some(credentials), host),
        None => (None, authority),
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return Err("only HTTP and HTTPS proxies are supported");
    }
    let origin = target_origin(&format!("{scheme}://{host}"))
        .ok()
        .flatten()
        .ok_or("invalid proxy URL")?;
    let decode = |part: &str| {
        percent_encoding::percent_decode_str(part)
            .decode_utf8_lossy()
            .into_owned()
    };
    let authorization = credentials.map(|credentials| {
        let (user, password) = credentials.split_once(':').unwrap_or((credentials, ""));
        format!(
            "Basic {}",
            STANDARD.encode(format!("{}:{}", decode(user), decode(password)))
        )
    });
    Ok(Upstream {
        origin,
        authorization,
    })
}

fn bypass(entry: &str) -> Option<Bypass> {
    let entry = entry.trim().to_ascii_lowercase();
    if entry == "*" {
        return Some(Bypass::All);
    }
    if let Ok(network) = entry.parse() {
        return Some(Bypass::Network(network));
    }
    if let Ok(address) = entry.parse() {
        return Some(Bypass::Address(address, None));
    }
    let (host, port) = match entry.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once(']')?,
        None => entry.split_once(':').unwrap_or((&entry, "")),
    };
    let port = match port.strip_prefix(':').unwrap_or(port) {
        "" => None,
        port => Some(port.parse().ok()?),
    };
    if let Ok(address) = host.parse() {
        return Some(Bypass::Address(address, port));
    }
    let host = host.strip_prefix('*').unwrap_or(host);
    let suffix = if host.starts_with('.') {
        host.to_owned()
    } else {
        format!(".{host}")
    };
    (suffix.len() > 1).then(|| Bypass::Domain {
        apex: !host.starts_with('.'),
        suffix,
        port,
    })
}

fn proxy_tls() -> Option<TlsConnector> {
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .ok()?
    .with_platform_verifier()
    .ok()?
    .with_no_client_auth();
    Some(TlsConnector::from(Arc::new(config)))
}

#[cfg(test)]
mod tests;

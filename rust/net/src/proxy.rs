//! Proxies from `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY`, chosen per target by Go's rules.
use base64::{Engine, engine::general_purpose::STANDARD};
use graphite_meter_proto::{
    idna,
    origin::{Host, Origin, Scheme},
};
use std::{fmt, net::IpAddr};

/// The proxies a process's environment names; `ALL_PROXY` is never read.
#[derive(Clone, Debug, Default)]
pub struct Proxy {
    http: Option<Result<Upstream, UnusableProxy>>,
    https: Option<Result<Upstream, UnusableProxy>>,
    bypass: Vec<Rule>,
    /// Under CGI, where a request's `Proxy` header sets `HTTP_PROXY`, so every cleartext request refuses it.
    cgi: bool,
}

/// A proxy variable this client cannot use; each request it would carry fails with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnusableProxy {
    variable: &'static str,
    reason: &'static str,
}

impl fmt::Display for UnusableProxy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} is not a usable proxy: {}", self.variable, self.reason)
    }
}

impl std::error::Error for UnusableProxy {}

/// A proxy a request goes through; it prints without its credentials.
#[derive(Clone)]
pub enum Upstream {
    /// An `http://` or `https://` proxy, with its `Proxy-Authorization` value.
    Http { origin: Origin, authorization: Option<String> },
    /// A `socks5://` or `socks5h://` proxy, which resolves host names itself, with its login.
    Socks { host: Host, port: u16, login: Option<(Vec<u8>, Vec<u8>)> },
}

impl Upstream {
    /// The proxy's own host and port.
    pub(crate) fn address(&self) -> (&Host, u16) {
        match self {
            Self::Http { origin, .. } => (&origin.host, origin.port),
            Self::Socks { host, port, .. } => (host, *port),
        }
    }
}

impl fmt::Display for Upstream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { origin, .. } => write!(formatter, "{origin}"),
            Self::Socks { host, port, .. } => write!(formatter, "socks5://{host}:{port}"),
        }
    }
}

impl fmt::Debug for Upstream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[derive(Clone, Debug)]
struct Rule {
    target: Bypass,
    /// The port as written, compared with the target's as text.
    port: Option<String>,
}

#[derive(Clone, Debug)]
enum Bypass {
    All,
    Network(ipnet::IpNet),
    Address(IpAddr),
    /// A name ending in `suffix`, which begins with a dot, or equal to it without the dot when `apex` is set.
    Domain {
        suffix: String,
        apex: bool,
    },
}

impl Proxy {
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Each variable before its lowercase spelling, empty values unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name: &'static str| Some((name, lookup(name).filter(|value| !value.is_empty())?));
        let read = |names: [&'static str; 2]| names.into_iter().find_map(set);
        let upstream = |names| {
            let (variable, raw) = read(names).filter(|(_, raw)| !raw.trim().is_empty())?;
            Some(upstream(raw.trim()).map_err(|reason| UnusableProxy { variable, reason }))
        };
        let mut proxy = Self {
            http: upstream(["HTTP_PROXY", "http_proxy"]),
            https: upstream(["HTTPS_PROXY", "https_proxy"]),
            bypass: read(["NO_PROXY", "no_proxy"]).map_or_else(Vec::new, |(_, list)| rules(&list)),
            cgi: false,
        };
        if let Some((variable, _)) = read(["HTTP_PROXY", "http_proxy"])
            && lookup("REQUEST_METHOD").is_some_and(|method| !method.is_empty())
        {
            let reason = "a CGI request's Proxy header can set it";
            proxy.http = Some(Err(UnusableProxy { variable, reason }));
            proxy.cgi = true;
        }
        proxy
    }

    /// The proxy `target` goes through, if any; under CGI a cleartext target refuses `HTTP_PROXY` before
    /// loopback and `NO_PROXY` apply.
    pub fn route(&self, target: &Origin) -> Result<Option<&Upstream>, UnusableProxy> {
        let (setting, bypassable) = match target.scheme {
            Scheme::Https => (&self.https, true),
            Scheme::Http => (&self.http, !self.cgi),
        };
        match setting {
            Some(_) if bypassable && self.bypassed(target) => Ok(None),
            Some(Ok(upstream)) => Ok(Some(upstream)),
            Some(Err(unusable)) => Err(unusable.clone()),
            None => Ok(None),
        }
    }

    fn bypassed(&self, target: &Origin) -> bool {
        let port = target.port.to_string();
        let (ip, name) = match &target.host {
            Host::Ip(ip) => (Some(ip.to_canonical()), None),
            Host::Name(name) => (None, Some(name.as_str())),
        };
        name == Some("localhost")
            || ip.is_some_and(|ip| ip.is_loopback())
            || self.bypass.iter().any(|rule| {
                rule.port.as_ref().is_none_or(|only| *only == port)
                    && match (&rule.target, ip, name) {
                        (Bypass::All, ..) => true,
                        (Bypass::Network(network), Some(ip), _) => network.contains(&ip),
                        (Bypass::Address(address), Some(ip), _) => *address == ip,
                        (Bypass::Domain { suffix, apex }, _, Some(name)) => {
                            name.ends_with(suffix.as_str()) || *apex && name == &suffix[1..]
                        }
                        _ => false,
                    }
            })
    }
}

/// A proxy URL, `http://` when it names no scheme.
fn upstream(raw: &str) -> Result<Upstream, &'static str> {
    const INVALID: &str = "invalid proxy URL";
    let (scheme, rest) = raw.split_once("://").unwrap_or(("http", raw));
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let (credentials, host) = match authority.rsplit_once('@') {
        Some((credentials, host)) => (Some(credentials.split_once(':').unwrap_or((credentials, ""))), host),
        None => (None, authority),
    };
    let decode = |part| percent_encoding::percent_decode_str(part).collect::<Vec<u8>>();
    match scheme.to_ascii_lowercase().as_str() {
        "socks5" | "socks5h" => {
            let origin = Origin::parse_received(&format!("http://{host}")).map_err(|_| INVALID)?;
            let port = if explicit_port(host) { origin.port } else { 1080 };
            let login = credentials.map(|(user, password)| (decode(user), decode(password)));
            Ok(Upstream::Socks { host: origin.host, port, login })
        }
        scheme @ ("http" | "https") => {
            let origin = Origin::parse_received(&format!("{scheme}://{host}")).map_err(|_| INVALID)?;
            let authorization = credentials.map(|(user, password)| {
                let pair = [decode(user), b":".to_vec(), decode(password)].concat();
                format!("Basic {}", STANDARD.encode(pair))
            });
            Ok(Upstream::Http { origin, authorization })
        }
        _ => Err("only HTTP, HTTPS and SOCKS5 proxies are supported"),
    }
}

/// Whether a `host[:port]` authority writes a port.
fn explicit_port(authority: &str) -> bool {
    let tail = authority.rsplit_once(']').map_or(authority, |(_, tail)| tail);
    tail.split_once(':').is_some_and(|(_, port)| !port.is_empty())
}

/// `NO_PROXY`'s entries; a lone `*` anywhere bypasses every target.
fn rules(list: &str) -> Vec<Rule> {
    let entries: Vec<String> = list.split(',').map(|entry| entry.trim().to_ascii_lowercase()).collect();
    if entries.iter().any(|entry| entry == "*") {
        return vec![Rule { target: Bypass::All, port: None }];
    }
    entries.iter().filter_map(|entry| rule(entry)).collect()
}

/// One entry; an entry that names no host a target can have is left out.
fn rule(entry: &str) -> Option<Rule> {
    let rule = |target, port: &str| Rule { target, port: (!port.is_empty()).then(|| port.to_owned()) };
    // A network's address reads as an IP address does, so a leading zero names no network.
    let address = entry.split_once('/').and_then(|(ip, _)| ip.parse::<IpAddr>().ok());
    if let Some(network) = address.and_then(|_| entry.parse().ok()) {
        return Some(rule(Bypass::Network(unmapped(network)), ""));
    }
    if let Ok(address) = entry.parse::<IpAddr>() {
        return Some(rule(Bypass::Address(address.to_canonical()), ""));
    }
    let (host, port) = match entry.strip_prefix('[') {
        // A bracketed host needs its port, which may be empty.
        Some(bracketed) => bracketed.split_once("]:")?,
        None => entry.split_once(':').unwrap_or((entry, "")),
    };
    if let Ok(address) = host.parse::<IpAddr>() {
        return Some(rule(Bypass::Address(address.to_canonical()), port));
    }
    // Only a leading `*.` loses its star, so `*example.com` names no host.
    let host = if host.starts_with("*.") { &host[1..] } else { host };
    let apex = !host.starts_with('.');
    let suffix = if apex { format!(".{host}") } else { host.to_owned() };
    let suffix = if suffix.is_ascii() { Some(suffix) } else { idna::to_ascii(&suffix) };
    Some(rule(Bypass::Domain { suffix: suffix.filter(|_| !host.is_empty())?, apex }, port))
}

/// An IPv4-mapped network as the IPv4 network it maps, which IPv4 targets fall in.
fn unmapped(network: ipnet::IpNet) -> ipnet::IpNet {
    let ipnet::IpNet::V6(v6) = network else {
        return network;
    };
    let mapped = v6.network().to_ipv4_mapped().zip(v6.prefix_len().checked_sub(96));
    mapped
        .and_then(|(v4, prefix)| ipnet::Ipv4Net::new(v4, prefix).ok())
        .map_or(network, ipnet::IpNet::V4)
}

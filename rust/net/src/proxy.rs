//! Reads proxies from `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY` and picks one per target.
use base64::{Engine, engine::general_purpose::STANDARD};
use graphite_meter_proto::origin::{Host, Origin, Scheme};
use std::{ffi::OsString, fmt, net::IpAddr};

/// Holds the proxies a process's environment names. `ALL_PROXY` is never read.
#[derive(Clone, Debug, Default)]
pub struct Proxy {
    http: Option<Result<Upstream, UnusableProxy>>,
    https: Option<Result<Upstream, UnusableProxy>>,
    bypass: Vec<Rule>,
}

/// Names a proxy variable this client cannot use. Each request it would carry fails with this error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnusableProxy {
    variable: &'static str,
    reason: &'static str,
}

impl fmt::Display for UnusableProxy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} is not a usable proxy: {}",
            self.variable, self.reason
        )
    }
}

impl std::error::Error for UnusableProxy {}

pub(crate) type Login = (Vec<u8>, Vec<u8>);

/// A request goes through this proxy. It prints without its credentials.
#[derive(Clone)]
pub enum Upstream {
    /// Names an `http://` or `https://` proxy with its `Proxy-Authorization` value.
    Http {
        origin: Origin,
        authorization: Option<String>,
    },
    /// Names a `socks5://` or `socks5h://` proxy with its login. The proxy resolves host names itself.
    Socks {
        host: Host,
        port: u16,
        login: Option<Login>,
    },
}

impl Upstream {
    pub(crate) fn address(&self) -> (&Host, u16) {
        match self {
            Self::Http { origin, .. } => (&origin.host, origin.port),
            Self::Socks { host, port, .. } => (host, *port),
        }
    }
}

impl fmt::Debug for Upstream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { origin, .. } => write!(formatter, "{origin}"),
            Self::Socks { host, port, .. } => write!(formatter, "socks5://{host}:{port}"),
        }
    }
}

#[derive(Clone, Debug)]
struct Rule {
    target: Bypass,
    port: Option<u16>,
}

#[derive(Clone, Debug)]
enum Bypass {
    All,
    Network(ipnet::IpNet),
    Address(IpAddr),
    /// Matches a name ending in `suffix`, which begins with a dot, or equal to it without the dot when `apex` is set.
    Domain {
        suffix: String,
        apex: bool,
    },
}

impl Proxy {
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var_os(name))
    }

    /// Reads each variable before its lowercase spelling, empty as unset; a non-UTF-8 `NO_PROXY` entry bypasses nothing.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<OsString>) -> Self {
        let set =
            |name: &'static str| Some((name, lookup(name).filter(|value| !value.is_empty())?));
        let read = |names: [&'static str; 2]| names.into_iter().find_map(set);
        let upstream = |names| {
            let (variable, raw) = read(names)?;
            let parsed = match raw.to_str().map(str::trim) {
                Some("") => return None,
                Some(raw) => upstream(raw),
                None => Err("not valid UTF-8"),
            };
            Some(parsed.map_err(|reason| UnusableProxy { variable, reason }))
        };
        Self {
            http: upstream(["HTTP_PROXY", "http_proxy"]),
            https: upstream(["HTTPS_PROXY", "https_proxy"]),
            bypass: read(["NO_PROXY", "no_proxy"])
                .map_or_else(Vec::new, |(_, list)| rules(&list.to_string_lossy())),
        }
    }

    pub fn route(&self, target: &Origin) -> Result<Option<&Upstream>, UnusableProxy> {
        let setting = match target.scheme {
            Scheme::Https => &self.https,
            Scheme::Http => &self.http,
        };
        match setting {
            Some(_) if self.bypassed(target) => Ok(None),
            Some(Ok(upstream)) => Ok(Some(upstream)),
            Some(Err(unusable)) => Err(unusable.clone()),
            None => Ok(None),
        }
    }

    fn bypassed(&self, target: &Origin) -> bool {
        let (ip, name) = match &target.host {
            Host::Ip(ip) => (Some(ip.to_canonical()), None),
            Host::Name(name) => (None, Some(name.as_str())),
        };
        name == Some("localhost")
            || ip.is_some_and(|ip| ip.is_loopback())
            || self.bypass.iter().any(|rule| {
                rule.port.is_none_or(|only| only == target.port)
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

/// Parses a proxy URL, assuming `http://` when it names no scheme.
fn upstream(raw: &str) -> Result<Upstream, &'static str> {
    const INVALID: &str = "invalid proxy URL";
    let (scheme, rest) = raw.split_once("://").unwrap_or(("http", raw));
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let (credentials, host) = match authority.rsplit_once('@') {
        Some((credentials, host)) => (
            Some(credentials.split_once(':').unwrap_or((credentials, ""))),
            host,
        ),
        None => (None, authority),
    };
    let decode = |part| percent_encoding::percent_decode_str(part).collect::<Vec<u8>>();
    match scheme.to_ascii_lowercase().as_str() {
        "socks5" | "socks5h" => {
            let origin = Origin::parse(&format!("http://{host}")).map_err(|_| INVALID)?;
            let port = if explicit_port(host) {
                origin.port
            } else {
                1080
            };
            let login = credentials.map(|(user, password)| (decode(user), decode(password)));
            Ok(Upstream::Socks {
                host: origin.host,
                port,
                login,
            })
        }
        scheme @ ("http" | "https") => {
            let origin = Origin::parse(&format!("{scheme}://{host}")).map_err(|_| INVALID)?;
            let authorization = credentials.map(|(user, password)| {
                let pair = [decode(user), b":".to_vec(), decode(password)].concat();
                format!("Basic {}", STANDARD.encode(pair))
            });
            Ok(Upstream::Http {
                origin,
                authorization,
            })
        }
        _ => Err("only HTTP, HTTPS and SOCKS5 proxies are supported"),
    }
}

fn explicit_port(authority: &str) -> bool {
    let tail = authority
        .rsplit_once(']')
        .map_or(authority, |(_, tail)| tail);
    tail.split_once(':')
        .is_some_and(|(_, port)| !port.is_empty())
}

/// Parses `NO_PROXY`'s entries. A lone `*` anywhere bypasses every target.
fn rules(list: &str) -> Vec<Rule> {
    let entries: Vec<String> = list
        .split(',')
        .map(|entry| entry.trim().to_ascii_lowercase())
        .collect();
    if entries.iter().any(|entry| entry == "*") {
        return vec![Rule {
            target: Bypass::All,
            port: None,
        }];
    }
    entries.iter().filter_map(|entry| rule(entry)).collect()
}

/// Parses one entry and drops one that names no host or port a target can have.
fn rule(entry: &str) -> Option<Rule> {
    let rule = |target, port: &str| match port {
        "" => Some(Rule { target, port: None }),
        port => port.parse().ok().map(|port| Rule {
            target,
            port: Some(port),
        }),
    };
    // A network's address reads as an IP address does, so a leading zero names no network.
    let address = entry
        .split_once('/')
        .and_then(|(ip, _)| ip.parse::<IpAddr>().ok());
    if let Some(network) = address.and_then(|_| entry.parse().ok()) {
        return rule(Bypass::Network(unmapped(network)), "");
    }
    if let Ok(address) = entry.parse::<IpAddr>() {
        return rule(Bypass::Address(address.to_canonical()), "");
    }
    let (host, port) = match entry.strip_prefix('[') {
        // A bracketed host needs its port, which may be empty.
        Some(bracketed) => bracketed.split_once("]:")?,
        None => entry.split_once(':').unwrap_or((entry, "")),
    };
    if let Ok(address) = host.parse::<IpAddr>() {
        return rule(Bypass::Address(address.to_canonical()), port);
    }
    // Only a leading `*.` loses its star, so `*example.com` names no host.
    let host = if host.starts_with("*.") {
        &host[1..]
    } else {
        host
    };
    let apex = !host.starts_with('.');
    if host.is_empty() || !host.is_ascii() {
        return None;
    }
    let suffix = if apex {
        format!(".{host}")
    } else {
        host.to_owned()
    };
    rule(Bypass::Domain { suffix, apex }, port)
}

/// Turns an IPv4-mapped network into the IPv4 network it maps, which IPv4 targets fall in.
fn unmapped(network: ipnet::IpNet) -> ipnet::IpNet {
    let ipnet::IpNet::V6(v6) = network else {
        return network;
    };
    let mapped = v6
        .network()
        .to_ipv4_mapped()
        .zip(v6.prefix_len().checked_sub(96));
    mapped
        .and_then(|(v4, prefix)| ipnet::Ipv4Net::new(v4, prefix).ok())
        .map_or(network, ipnet::IpNet::V4)
}

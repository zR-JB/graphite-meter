//! Attribute proxy headers only through an explicitly trusted socket peer.
use std::net::{IpAddr, SocketAddr};

use graphite_meter_core::discovery::ClientIpSource;
use http::HeaderMap;
use ipnet::IpNet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientAddress {
    pub addr: IpAddr,
    pub source: ClientIpSource,
    pub usable: bool,
}
impl ClientAddress {
    pub fn version(self) -> u8 {
        if self.addr.is_ipv4() { 4 } else { 6 }
    }

    pub fn anonymous_key(self) -> String {
        client_keys(self.addr).remove(0)
    }
}

pub fn client_keys(addr: IpAddr) -> Vec<String> {
    match addr.to_canonical() {
        IpAddr::V4(addr) => vec![addr.to_string()],
        IpAddr::V6(addr) => [64, 56, 48]
            .into_iter()
            .map(|bits| {
                ipnet::Ipv6Net::new(addr, bits)
                    .expect("valid prefix length")
                    .trunc()
                    .to_string()
            })
            .collect(),
    }
}

pub fn share_full(keys: &[String], limit: usize, held: impl Fn(&str) -> usize) -> bool {
    keys.iter()
        .enumerate()
        .any(|(index, key)| held(key) >= limit.saturating_mul(1 << index))
}

pub fn resolve(peer: SocketAddr, headers: &HeaderMap, trusted: &[IpNet]) -> ClientAddress {
    let peer = peer.ip().to_canonical();
    let mut client = ClientAddress {
        addr: peer,
        source: ClientIpSource::Socket,
        usable: true,
    };
    if !trusted.iter().any(|prefix| prefix.contains(&peer)) {
        return client;
    }
    client.usable = false;
    if ["forwarded", "x-forwarded-for"]
        .into_iter()
        .any(|name| headers.get(name).is_some_and(|value| !value.is_empty()))
    {
        return client;
    }
    let mut values = headers.get_all("x-real-ip").iter();
    if let Some(value) = values.next()
        && values.next().is_none()
        && let Ok(value) = value.to_str()
        && let Ok(addr) = value.trim().parse::<IpAddr>()
    {
        client.addr = addr.to_canonical();
        client.source = ClientIpSource::Forwarded;
        client.usable = true;
    }
    client
}

//! Attribute proxy headers only through an explicitly trusted socket peer.
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
};

use graphite_meter_core::discovery::ClientIpSource;
use http::{HeaderMap, HeaderValue, header::AsHeaderName};
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

#[derive(Default)]
pub struct Shares(HashMap<String, usize>);

impl Shares {
    pub fn full(&self, keys: &[String], limit: usize) -> bool {
        share_full(keys, limit, |key| self.0.get(key).copied().unwrap_or_default())
    }

    /// Released keys are removed, so any entry holds something.
    pub fn holds_any(&self, keys: &[String]) -> bool {
        keys.iter().any(|key| self.0.contains_key(key))
    }

    pub fn hold(&mut self, keys: &[String]) {
        for key in keys {
            *self.0.entry(key.clone()).or_default() += 1;
        }
    }

    pub fn release(&mut self, keys: &[String]) {
        for key in keys {
            let held = self.0.get_mut(key).expect("released keys were held");
            *held -= 1;
            if *held == 0 {
                self.0.remove(key);
            }
        }
    }
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
    if let Some(value) = unique_header(headers, "x-real-ip")
        && let Ok(value) = value.to_str()
        && let Ok(addr) = value.trim().parse::<IpAddr>()
    {
        client.addr = addr.to_canonical();
        client.source = ClientIpSource::Forwarded;
        client.usable = true;
    }
    client
}

pub fn unique_header(headers: &HeaderMap, name: impl AsHeaderName) -> Option<&HeaderValue> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

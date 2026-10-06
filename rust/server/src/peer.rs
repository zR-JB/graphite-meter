//! Who a request comes from: socket or trusted-proxy address, sign-in, and the client keys limits and uploads use.

use crate::auth::{AuthLease, Holder};
use graphite_meter_proto::discovery::ClientIpSource;
use http::HeaderMap;
use ipnet::{IpNet, Ipv6Net};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::Arc,
};

/// A client's address and where it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    /// The socket peer, which is no trusted proxy.
    Socket(IpAddr),
    /// The single `X-Real-IP` a trusted proxy sent.
    Forwarded(IpAddr),
    /// A trusted proxy's request naming no single client; the address is the proxy's.
    Ambiguous(IpAddr),
}

impl Address {
    /// A trusted peer's client is its one parsable `X-Real-IP` absent `Forwarded`/`X-Forwarded-For`; else the peer.
    pub fn resolve(socket: IpAddr, headers: &HeaderMap, trusted: &[IpNet]) -> Self {
        let socket = socket.to_canonical();
        if !is_trusted(socket, trusted) {
            return Self::Socket(socket);
        }
        let mut forwarding = ["forwarded", "x-forwarded-for"].into_iter();
        let chained = forwarding.any(|name| headers.get(name).is_some_and(|value| !value.is_empty()));
        let mut real = headers.get_all("x-real-ip").iter();
        let named = match (real.next(), real.next()) {
            (Some(value), None) if !chained => value.to_str().ok().and_then(|text| text.trim().parse().ok()),
            _ => None,
        };
        named.map_or(Self::Ambiguous(socket), |client: IpAddr| Self::Forwarded(client.to_canonical()))
    }

    pub fn ip(self) -> IpAddr {
        match self {
            Self::Socket(ip) | Self::Forwarded(ip) | Self::Ambiguous(ip) => ip,
        }
    }

    /// Where a usable address was read; `None` when it is ambiguous.
    pub fn source(self) -> Option<ClientIpSource> {
        match self {
            Self::Socket(_) => Some(ClientIpSource::Socket),
            Self::Forwarded(_) => Some(ClientIpSource::Forwarded),
            Self::Ambiguous(_) => None,
        }
    }
}

fn is_trusted(ip: IpAddr, trusted: &[IpNet]) -> bool {
    trusted.iter().any(|prefix| prefix.contains(&ip))
}

/// One request's client: its address and, when signed in, its lease.
#[derive(Debug, Clone)]
pub struct Peer {
    address: Address,
    auth: Option<AuthLease>,
}

impl Peer {
    pub fn new(address: Address) -> Self {
        Self { address, auth: None }
    }

    pub fn with_auth(self, auth: Option<AuthLease>) -> Self {
        Self { auth, ..self }
    }

    pub fn address(&self) -> Address {
        self.address
    }

    pub fn auth(&self) -> Option<&AuthLease> {
        self.auth.as_ref()
    }

    /// The identity limits charge and uploads belong to: sign-in, else address; `None` for ambiguous proxy evidence.
    pub fn keys(&self) -> Option<ClientKeys> {
        match (&self.auth, self.address) {
            (Some(auth), _) => Some(auth.keys()),
            (None, Address::Ambiguous(_)) => None,
            (None, address) => Some(ClientKeys::address(address.ip())),
        }
    }
}

/// One key a client's share is counted under.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientKey {
    V4(Ipv4Addr),
    V6(Ipv6Net),
    Holder(Holder),
    Principal(Arc<str>),
}

/// A client's keys, narrowest first; each holds twice the share of the one before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientKeys {
    /// No keys, charging only totals: the connections of a trusted proxy.
    Exempt,
    V4(Ipv4Addr),
    /// An IPv6 client: its /64, then the /56 and /48 holding it.
    V6(Ipv6Addr),
    /// A login or grant, then the principal it belongs to.
    Auth(Holder, Arc<str>),
}

impl ClientKeys {
    pub fn address(ip: IpAddr) -> Self {
        match ip.to_canonical() {
            IpAddr::V4(ip) => Self::V4(ip),
            IpAddr::V6(ip) => Self::V6(ip),
        }
    }

    /// The keys a connection counts under: its socket address, or none for a trusted proxy.
    pub fn connection(socket: IpAddr, trusted: &[IpNet]) -> Self {
        let socket = socket.to_canonical();
        if is_trusted(socket, trusted) { Self::Exempt } else { Self::address(socket) }
    }

    /// The key an owner is compared by: the address, IPv6 /64, login or grant.
    pub fn narrowest(&self) -> Option<ClientKey> {
        self.iter().next()
    }

    pub fn iter(&self) -> impl Iterator<Item = ClientKey> {
        let prefix = |ip, bits| ClientKey::V6(Ipv6Net::new(ip, bits).expect("a prefix length up to 128").trunc());
        let keys = match self {
            Self::Exempt => [None, None, None],
            Self::V4(ip) => [Some(ClientKey::V4(*ip)), None, None],
            Self::V6(ip) => [64, 56, 48].map(|bits| Some(prefix(*ip, bits))),
            Self::Auth(holder, principal) => [
                Some(ClientKey::Holder(holder.clone())),
                Some(ClientKey::Principal(principal.clone())),
                None,
            ],
        };
        keys.into_iter().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Store;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn only_a_trusted_peer_names_its_client_and_only_unambiguously() {
        fn resolve(socket: &str, pairs: &[(&'static str, &'static str)]) -> Address {
            Address::resolve(socket.parse().unwrap(), &headers(pairs), &["10.0.0.0/8".parse().unwrap()])
        }
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        let real = ("x-real-ip", " 2001:db8::1 ");
        assert_eq!(resolve("::ffff:192.0.2.1", &[real]), Address::Socket(ip("192.0.2.1")));
        assert_eq!(resolve("10.1.2.3", &[real]), Address::Forwarded(ip("2001:db8::1")));
        assert_eq!(
            resolve("::ffff:10.1.2.3", &[("x-real-ip", "::ffff:192.0.2.9")]),
            Address::Forwarded(ip("192.0.2.9"))
        );
        let proxy = Address::Ambiguous(ip("10.1.2.3"));
        for pairs in [
            &[][..],
            &[real, real],
            &[("x-real-ip", "192.0.2.1, 192.0.2.2")],
            &[("x-real-ip", "unknown")],
            &[real, ("forwarded", "for=192.0.2.1")],
            &[real, ("x-forwarded-for", "192.0.2.1")],
        ] {
            assert_eq!(resolve("10.1.2.3", pairs), proxy, "{pairs:?}");
        }
        assert_eq!(
            resolve("10.1.2.3", &[real, ("x-forwarded-for", "")]),
            Address::Forwarded(ip("2001:db8::1"))
        );
        assert_eq!(proxy.source(), None);
        assert_eq!(Address::Forwarded(ip("::1")).source(), Some(ClientIpSource::Forwarded));
    }

    #[test]
    fn keys_widen_from_the_narrowest_identity() {
        let keys = |keys: ClientKeys| keys.iter().collect::<Vec<_>>();
        let v6 = keys(ClientKeys::address("2001:db8:1:2:3::4".parse().unwrap()));
        let nets = ["2001:db8:1:2::/64", "2001:db8:1::/56", "2001:db8:1::/48"];
        assert_eq!(v6, nets.map(|net| ClientKey::V6(net.parse().unwrap())));
        let v4 = keys(ClientKeys::address("::ffff:192.0.2.1".parse().unwrap()));
        assert_eq!(v4, [ClientKey::V4(Ipv4Addr::new(192, 0, 2, 1))]);
        let store = Store::default();
        let login = store.sign_in("alice", "Alice", "local").unwrap();
        let lease = store.bearer(&store.grant(login.key, None).unwrap()).unwrap();
        let ClientKeys::Auth(Holder::Grant(grant), _) = lease.keys() else {
            panic!("a grant's lease")
        };
        let peer = Peer::new(Address::Ambiguous("10.0.0.1".parse().unwrap()));
        assert_eq!(peer.keys(), None, "ambiguous evidence owns nothing");
        let signed_in = keys(peer.with_auth(Some(lease)).keys().unwrap());
        assert_eq!(signed_in, [ClientKey::Holder(Holder::Grant(grant)), ClientKey::Principal("alice".into())]);
        let trusted = ["192.0.2.0/24".parse().unwrap()];
        assert_eq!(ClientKeys::connection("192.0.2.7".parse().unwrap(), &trusted), ClientKeys::Exempt);
        assert_eq!(keys(ClientKeys::Exempt), []);
    }
}

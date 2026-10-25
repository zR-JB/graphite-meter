//! Names a request's client by socket or trusted-proxy address and sign-in, and the client keys limits and uploads use.

use crate::admission::Lease;
use crate::auth::{AuthLease, Holder, Principal};
use crate::log::{Level, RateLimited};
use graphite_meter_proto::discovery::ClientIpSource;
use http::{HeaderMap, HeaderName};
use ipnet::{IpNet, Ipv6Net};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

/// A misconfigured proxy refuses every request it forwards, so one line a minute names the fault.
static REFUSALS: RateLimited = RateLimited::new(
    Level::Warn,
    "proxy",
    "refused proxy requests",
    module_path!(),
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    /// The socket peer is no trusted proxy.
    Socket(IpAddr),
    /// A trusted proxy sent this single `X-Real-IP`.
    Forwarded(IpAddr),
    /// A trusted proxy's request named no single client, so the address is the proxy's.
    Ambiguous(IpAddr),
}

impl Address {
    /// A trusted peer's client is its one `X-Real-IP` (see `forwarded_client`). Any other peer is the client itself.
    pub fn resolve(socket: IpAddr, headers: &HeaderMap, trusted: &[IpNet]) -> Self {
        let socket = socket.to_canonical();
        if !is_trusted(socket, trusted) {
            return Self::Socket(socket);
        }
        match forwarded_client(headers) {
            Ok(client) => Self::Forwarded(client),
            Err(fault) => {
                REFUSALS.write(format_args!(
                    "request from trusted proxy {socket} names no client: {fault}; set the proxy to overwrite \
                     X-Real-IP with the address it accepted the connection from (docs/DEPLOYMENT.md, Reverse proxies)"
                ));
                Self::Ambiguous(socket)
            }
        }
    }

    pub fn ip(self) -> IpAddr {
        match self {
            Self::Socket(ip) | Self::Forwarded(ip) | Self::Ambiguous(ip) => ip,
        }
    }

    pub fn source(self) -> Option<ClientIpSource> {
        match self {
            Self::Socket(_) => Some(ClientIpSource::Socket),
            Self::Forwarded(_) => Some(ClientIpSource::Forwarded),
            Self::Ambiguous(_) => None,
        }
    }
}

/// `X-Real-IP` must end any `X-Forwarded-For`; `Forwarded` is ignored, as some proxies pass a client's on.
fn forwarded_client(headers: &HeaderMap) -> Result<IpAddr, String> {
    let mut real = headers.get_all(crate::headers::X_REAL_IP).iter();
    let (Some(value), None) = (real.next(), real.next()) else {
        return Err("the proxy must send X-Real-IP once".into());
    };
    let client = value
        .to_str()
        .ok()
        .and_then(|text| text.trim().parse::<IpAddr>().ok())
        .ok_or("X-Real-IP is not one IP address")?
        .to_canonical();
    if let Some(chain) = headers
        .get_all(crate::headers::X_FORWARDED_FOR)
        .iter()
        .next_back()
    {
        let chain = chain.to_str().map_err(|_| "X-Forwarded-For is not text")?;
        let last = chain.rsplit(',').next().unwrap_or_default().trim();
        if !last.is_empty() && hop(last) != Some(client) {
            return Err(format!(
                "X-Forwarded-For ends in {last}, not X-Real-IP {client}"
            ));
        }
    }
    Ok(client)
}

/// Parses an `X-Forwarded-For` entry, which some proxies write with a port.
fn hop(text: &str) -> Option<IpAddr> {
    let ip = text
        .parse::<IpAddr>()
        .or_else(|_| text.parse::<SocketAddr>().map(|address| address.ip()));
    ip.ok().map(|ip| ip.to_canonical())
}

/// Reads a field that is present once with one comma-free value.
pub(crate) fn single<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .filter(|_| values.next().is_none())?
        .to_str()
        .ok()?;
    (!value.contains(',')).then(|| value.trim())
}

fn is_trusted(ip: IpAddr, trusted: &[IpNet]) -> bool {
    trusted.iter().any(|prefix| prefix.contains(&ip))
}

#[derive(Debug, Clone)]
pub struct Peer {
    address: Address,
    auth: Option<AuthLease>,
    lease: Option<Arc<Lease>>,
}

impl Peer {
    pub fn new(address: Address) -> Self {
        Self {
            address,
            auth: None,
            lease: None,
        }
    }

    pub fn with_auth(self, auth: Option<AuthLease>) -> Self {
        Self { auth, ..self }
    }

    pub fn with_lease(self, lease: Option<Arc<Lease>>) -> Self {
        Self { lease, ..self }
    }

    pub fn lease(&self) -> Option<&Arc<Lease>> {
        self.lease.as_ref()
    }

    pub fn address(&self) -> Address {
        self.address
    }

    pub fn auth(&self) -> Option<&AuthLease> {
        self.auth.as_ref()
    }

    /// Tells whether a trusted proxy says the client connected over TLS. Traefik names a WebSocket upgrade's TLS `wss`.
    pub fn forwarded_tls(&self, headers: &HeaderMap) -> bool {
        let proto = single(headers, &crate::headers::X_FORWARDED_PROTO);
        !matches!(self.address, Address::Socket(_)) && matches!(proto, Some("https" | "wss"))
    }

    /// Uploads belong to the lease presented, else to the client `keys` names.
    pub fn owner(&self) -> Option<ClientKeys> {
        match &self.lease {
            Some(lease) => Some(ClientKeys::Lease(lease.id())),
            None => self.keys(),
        }
    }

    /// Limits charge the sign-in or else the address. Ambiguous proxy evidence gives `None`.
    pub fn keys(&self) -> Option<ClientKeys> {
        match (&self.auth, self.address) {
            (Some(auth), _) => Some(auth.keys()),
            (None, Address::Ambiguous(_)) => None,
            (None, address) => Some(ClientKeys::address(address.ip())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientKey {
    V4(Ipv4Addr),
    V6(Ipv6Net),
    Holder(Holder),
    Principal(Principal),
    Lease(u64),
}

/// Lists a client's keys, narrowest first. Each holds twice the share of the one before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientKeys {
    /// A trusted proxy's connections carry no keys and charge only totals.
    Exempt,
    V4(Ipv4Addr),
    /// An IPv6 client keys its /64, then the /56 and /48 holding it.
    V6(Ipv6Addr),
    /// A login or grant comes first, then the principal it belongs to.
    Auth(Holder, Principal),
    /// Work presenting a lease, which holds its admission already.
    Lease(u64),
}

impl ClientKeys {
    pub fn address(ip: IpAddr) -> Self {
        match ip.to_canonical() {
            IpAddr::V4(ip) => Self::V4(ip),
            IpAddr::V6(ip) => Self::V6(ip),
        }
    }

    pub fn connection(socket: IpAddr, trusted: &[IpNet]) -> Self {
        let socket = socket.to_canonical();
        if is_trusted(socket, trusted) {
            Self::Exempt
        } else {
            Self::address(socket)
        }
    }

    /// Returns the key an owner is compared by, which is the address, IPv6 /64, login or grant.
    pub fn narrowest(&self) -> Option<ClientKey> {
        self.iter().next()
    }

    pub fn shares(&self, narrowest: usize) -> impl Iterator<Item = (ClientKey, usize)> {
        let doubled = std::iter::successors(Some(narrowest), |share| Some(share.saturating_mul(2)));
        self.iter().zip(doubled)
    }

    pub fn iter(&self) -> impl Iterator<Item = ClientKey> {
        let prefix = |ip, bits| {
            ClientKey::V6(
                Ipv6Net::new(ip, bits)
                    .expect("a prefix length up to 128")
                    .trunc(),
            )
        };
        let keys = match self {
            Self::Exempt => [None, None, None],
            Self::V4(ip) => [Some(ClientKey::V4(*ip)), None, None],
            Self::V6(ip) => [64, 56, 48].map(|bits| Some(prefix(*ip, bits))),
            Self::Auth(holder, principal) => [
                Some(ClientKey::Holder(holder.clone())),
                Some(ClientKey::Principal(principal.clone())),
                None,
            ],
            Self::Lease(id) => [Some(ClientKey::Lease(*id)), None, None],
        };
        keys.into_iter().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Identity, Store};

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
            Address::resolve(
                socket.parse().unwrap(),
                &headers(pairs),
                &["10.0.0.0/8".parse().unwrap()],
            )
        }
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        let real = ("x-real-ip", " 2001:db8::1 ");
        assert_eq!(
            resolve("::ffff:192.0.2.1", &[real]),
            Address::Socket(ip("192.0.2.1"))
        );
        assert_eq!(
            resolve("10.1.2.3", &[real]),
            Address::Forwarded(ip("2001:db8::1"))
        );
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
            // A proxy that passed a client's X-Real-IP on still appended the address it saw.
            &[real, ("x-forwarded-for", "2001:db8::1, 192.0.2.1")],
            &[real, ("x-forwarded-for", "unknown")],
            &[("x-forwarded-for", "2001:db8::1")],
        ] {
            assert_eq!(resolve("10.1.2.3", pairs), proxy, "{pairs:?}");
        }
        // Traefik, Caddy and nginx append the peer they saw; a client's own Forwarded passes some proxies untouched.
        for pairs in [
            &[real, ("x-forwarded-for", "")][..],
            &[real, ("x-forwarded-for", "192.0.2.1, 2001:db8::1")],
            &[
                real,
                ("x-forwarded-for", "192.0.2.1"),
                ("x-forwarded-for", "[2001:db8::1]:443"),
            ],
            &[real, ("forwarded", "for=192.0.2.1")],
        ] {
            assert_eq!(
                resolve("10.1.2.3", pairs),
                Address::Forwarded(ip("2001:db8::1")),
                "{pairs:?}"
            );
        }
        assert_eq!(proxy.source(), None);
        assert_eq!(
            Address::Forwarded(ip("::1")).source(),
            Some(ClientIpSource::Forwarded)
        );
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
        let identity = Identity {
            principal: Principal::oidc("alice"),
            name: "Alice",
            provider: "local",
        };
        let login = store.sign_in(identity).unwrap();
        let lease = store
            .bearer(store.grant(login.key, None).unwrap().as_str())
            .unwrap();
        let ClientKeys::Auth(Holder::Grant(grant), _) = lease.keys() else {
            panic!("a grant's lease")
        };
        let peer = Peer::new(Address::Ambiguous("10.0.0.1".parse().unwrap()));
        assert_eq!(peer.keys(), None, "ambiguous evidence owns nothing");
        let signed_in = keys(peer.with_auth(Some(lease)).keys().unwrap());
        assert_eq!(
            signed_in,
            [
                ClientKey::Holder(Holder::Grant(grant)),
                ClientKey::Principal(Principal::oidc("alice"))
            ]
        );
        let trusted = ["192.0.2.0/24".parse().unwrap()];
        assert_eq!(
            ClientKeys::connection("192.0.2.7".parse().unwrap(), &trusted),
            ClientKeys::Exempt
        );
        assert_eq!(keys(ClientKeys::Exempt), []);
    }
}

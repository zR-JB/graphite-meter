//! Operating-system and network plumbing shared by the server and the client.
mod connect;
mod crypto;
mod dial;
mod proxy;
pub mod quic;
mod runtime;
mod socks;
mod trust;
mod udp;

pub use connect::{ConnectError, Connection, Connector, RequestForm, Stream};
pub use crypto::{Alpn, Verify, client_config, provider};
pub use dial::resolve;
pub use proxy::{Proxy, UnusableProxy, Upstream};
pub use runtime::Pool;
pub use udp::bind_udp;

//! Operating-system and network plumbing shared by the server and the client.
mod connect;
mod crypto;
mod dial;
mod proxy;
mod runtime;
mod socks;
mod trust;
mod udp;

pub use connect::{ConnectError, Connection, Connector, RequestForm, Stream};
pub use crypto::{Verify, client_config, provider};
pub use dial::resolve;
pub use proxy::{Proxy, UnusableProxy, Upstream};
pub use runtime::Pool;
pub use udp::bind_udp;

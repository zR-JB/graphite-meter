//! Operating-system and network plumbing shared by the server and the client.
mod connect;
mod crypto;
mod dial;
mod proxy;
pub mod quic;
mod runtime;
mod trust;
mod udp;

pub use connect::{ConnectError, Connection, Connector, RequestForm, Stream, TLS_READ_BYTES};
pub use crypto::{Verify, aes_hardware, client_config, provider};
pub use proxy::{Proxy, UnusableProxy};
pub use runtime::Pool;
pub use trust::Trust;
pub use udp::bind_udp;

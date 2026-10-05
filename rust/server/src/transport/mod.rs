//! What carries requests and replies over the network.

pub mod accept;
pub mod body;
pub mod http1;
pub mod http2;
pub mod lifecycle;
pub mod quic;
pub mod tls;
pub mod websocket;
pub mod webtransport;

use crate::log::RateLimited;

/// Connection failures any peer can cause, such as a refused TLS handshake.
pub(crate) static PEER_FAILURES: RateLimited = RateLimited::new("peer connection failures");

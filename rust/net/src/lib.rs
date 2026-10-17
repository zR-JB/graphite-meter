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

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `mutex`, recovering it after a reported panic elsewhere, so one bug never fails every later user.
pub fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `N` bytes of system randomness; a host without it can neither serve nor measure, so it panics.
pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).expect("the system provides randomness");
    bytes
}

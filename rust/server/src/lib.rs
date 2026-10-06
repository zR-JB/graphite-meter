//! The Graphite Meter server.
// Refusals are responses, no larger than the outcomes they replace.
#![allow(clippy::result_large_err)]

pub mod app;
pub mod assets;
pub mod auth;
mod clock;
pub mod config;
pub mod engine;
pub mod exchange;
pub mod lane;
pub mod limits;
pub mod log;
pub mod peer;
pub mod runtime;
pub mod transport;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `mutex`, recovering it after a reported panic elsewhere, so one bug never fails every later user.
pub(crate) fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `N` bytes of system randomness; startup draws some first, so a later failure is a broken host and panics.
pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).expect("the system's randomness serves every credential");
    bytes
}

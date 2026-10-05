//! The Graphite Meter server.

pub mod auth;
pub mod config;
pub mod lane;
pub mod limits;
pub mod log;
pub mod peer;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `mutex`, recovering its state after a panic elsewhere held it: that panic was reported, and failing every
/// later user would turn one bug into an outage.
pub(crate) fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

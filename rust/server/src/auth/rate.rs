//! Bounded rolling-window budgets for authentication attempts.
use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::Mutex,
    time::Duration,
};

use tokio::time::Instant;

const WINDOW: Duration = Duration::from_secs(60);
const MAX_KEYS: usize = 2048;
const PASSWORD_ADDRESS_LIMIT: usize = 5;
const PASSWORD_GLOBAL_LIMIT: usize = 60;
const EXCHANGE_ADDRESS_LIMIT: usize = 10;
const APPROVAL_ADDRESS_LIMIT: usize = 10;

type Attempts = VecDeque<Instant>;
type AddressAttempts = HashMap<String, Attempts>;

#[derive(Clone, Copy)]
pub enum Budget {
    Password,
    OidcExchange,
    BrowserApproval,
}

#[derive(Default)]
struct State {
    password: AddressAttempts,
    global_password: Attempts,
    exchanges: AddressAttempts,
    approvals: AddressAttempts,
}

/// Separate address budgets share one short lock so password address/global
/// checks commit atomically. Callers supply an address resolved by proxy policy.
#[derive(Default)]
pub struct AttemptLimiter {
    state: Mutex<State>,
}

impl AttemptLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow(&self, budget: Budget, address: IpAddr) -> bool {
        let keys = crate::client_address::client_keys(address);
        let mut state = self.state.lock().expect("auth attempt mutex poisoned");
        // Sample after acquiring the lock to keep stored timestamps ordered.
        let now = Instant::now();
        match budget {
            Budget::Password => {
                if !address_has_room(&mut state.password, &keys, PASSWORD_ADDRESS_LIMIT, now) {
                    return false;
                }
                expire(&mut state.global_password, now);
                if state.global_password.len() >= PASSWORD_GLOBAL_LIMIT {
                    return false;
                }
                for key in &keys {
                    state
                        .password
                        .entry(key.clone())
                        .or_default()
                        .push_back(now);
                }
                state.global_password.push_back(now);
            }
            Budget::OidcExchange => {
                if !address_has_room(&mut state.exchanges, &keys, EXCHANGE_ADDRESS_LIMIT, now) {
                    return false;
                }
                for key in &keys {
                    state
                        .exchanges
                        .entry(key.clone())
                        .or_default()
                        .push_back(now);
                }
            }
            Budget::BrowserApproval => {
                if !address_has_room(&mut state.approvals, &keys, APPROVAL_ADDRESS_LIMIT, now) {
                    return false;
                }
                for key in &keys {
                    state
                        .approvals
                        .entry(key.clone())
                        .or_default()
                        .push_back(now);
                }
            }
        }
        true
    }
}

fn address_has_room(
    addresses: &mut AddressAttempts,
    keys: &[String],
    limit: usize,
    now: Instant,
) -> bool {
    addresses.retain(|_, attempts| {
        expire(attempts, now);
        !attempts.is_empty()
    });
    let missing = keys
        .iter()
        .filter(|key| !addresses.contains_key(*key))
        .count();
    addresses.len() + missing <= MAX_KEYS
        && !crate::client_address::share_full(keys, limit, |key| {
            addresses.get(key).map_or(0, Attempts::len)
        })
}

fn expire(attempts: &mut Attempts, now: Instant) {
    while attempts
        .front()
        .is_some_and(|&time| now.duration_since(time) >= WINDOW)
    {
        attempts.pop_front();
    }
}

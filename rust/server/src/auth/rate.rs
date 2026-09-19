//! Bounded rolling-window budgets for authentication attempts.
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv6Addr},
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
type AddressAttempts = HashMap<IpAddr, Attempts>;

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
        let key = address_key(address);
        let mut state = self.state.lock().expect("auth attempt mutex poisoned");
        // Sample after acquiring the lock to keep stored timestamps ordered.
        let now = Instant::now();
        match budget {
            Budget::Password => {
                if !address_has_room(&mut state.password, key, PASSWORD_ADDRESS_LIMIT, now) {
                    return false;
                }
                expire(&mut state.global_password, now);
                if state.global_password.len() >= PASSWORD_GLOBAL_LIMIT {
                    return false;
                }
                state.password.entry(key).or_default().push_back(now);
                state.global_password.push_back(now);
            }
            Budget::OidcExchange => {
                if !address_has_room(&mut state.exchanges, key, EXCHANGE_ADDRESS_LIMIT, now) {
                    return false;
                }
                state.exchanges.entry(key).or_default().push_back(now);
            }
            Budget::BrowserApproval => {
                if !address_has_room(&mut state.approvals, key, APPROVAL_ADDRESS_LIMIT, now) {
                    return false;
                }
                state.approvals.entry(key).or_default().push_back(now);
            }
        }
        true
    }
}

fn address_has_room(
    addresses: &mut AddressAttempts,
    key: IpAddr,
    limit: usize,
    now: Instant,
) -> bool {
    if let Some(attempts) = addresses.get_mut(&key) {
        expire(attempts, now);
        return attempts.len() < limit;
    }
    if addresses.len() >= MAX_KEYS {
        addresses.retain(|_, attempts| {
            expire(attempts, now);
            !attempts.is_empty()
        });
    }
    addresses.len() < MAX_KEYS
}

fn expire(attempts: &mut Attempts, now: Instant) {
    while attempts
        .front()
        .is_some_and(|&time| now.duration_since(time) >= WINDOW)
    {
        attempts.pop_front();
    }
}

fn address_key(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return mapped.into();
            }
            let mut prefix = address.octets();
            prefix[8..].fill(0);
            Ipv6Addr::from(prefix).into()
        }
    }
}

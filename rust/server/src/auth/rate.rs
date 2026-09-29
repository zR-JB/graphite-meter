//! Bounded rolling-window budgets for authentication attempts.
use super::logging::{Ceiling, SecurityLog};
use crate::sync::lock;
use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::time::Instant;

const WINDOW: Duration = Duration::from_secs(60);
const MAX_KEYS: usize = 2048;
const PASSWORD_ADDRESS_LIMIT: usize = 5;
const PASSWORD_GLOBAL_LIMIT: usize = 60;
const EXCHANGE_ADDRESS_LIMIT: usize = 10;
const OIDC_START_ADDRESS_LIMIT: usize = 10;
const APPROVAL_ADDRESS_LIMIT: usize = 10;

type Attempts = VecDeque<Instant>;
type AddressAttempts = HashMap<String, Attempts>;

#[derive(Clone, Copy)]
pub enum Budget {
    Password,
    KnownDevice,
    OidcExchange,
    OidcStart,
    BrowserApproval,
}

#[derive(Default)]
struct State {
    password: AddressAttempts,
    failed_passwords: Attempts,
    exchanges: AddressAttempts,
    starts: AddressAttempts,
    approvals: AddressAttempts,
}

/// Address budgets share one short lock; callers supply an address resolved by proxy policy.
#[derive(Default)]
pub struct AttemptLimiter {
    state: Mutex<State>,
    log: Arc<SecurityLog>,
}

impl AttemptLimiter {
    pub(super) fn with_log(log: Arc<SecurityLog>) -> Self {
        Self {
            state: Mutex::default(),
            log,
        }
    }

    pub fn allow(&self, budget: Budget, address: IpAddr) -> bool {
        let keys = crate::client_address::client_keys(address);
        let mut state = lock(&self.state);
        // Sample after acquiring the lock to keep stored timestamps ordered.
        let now = Instant::now();
        let State {
            password,
            failed_passwords,
            exchanges,
            starts,
            approvals,
        } = &mut *state;
        let (addresses, limit, ceiling) = match budget {
            Budget::Password | Budget::KnownDevice => (password, PASSWORD_ADDRESS_LIMIT, Ceiling::PasswordAddress),
            Budget::OidcExchange => (exchanges, EXCHANGE_ADDRESS_LIMIT, Ceiling::ExchangeAddress),
            Budget::OidcStart => (starts, OIDC_START_ADDRESS_LIMIT, Ceiling::StartAddress),
            Budget::BrowserApproval => (approvals, APPROVAL_ADDRESS_LIMIT, Ceiling::ApprovalAddress),
        };
        let known = matches!(budget, Budget::KnownDevice);
        addresses.retain(|_, attempts| {
            expire(attempts, now);
            !attempts.is_empty()
        });
        let missing = keys.iter().filter(|key| !addresses.contains_key(*key)).count();
        let full = addresses.len() + missing > MAX_KEYS;
        if full && !known {
            self.log.ceiling(ceiling);
            return false;
        }
        if crate::client_address::share_full(&keys, limit, |key| addresses.get(key).map_or(0, Attempts::len)) {
            return false;
        }
        if matches!(budget, Budget::Password) {
            expire(failed_passwords, now);
            if failed_passwords.len() >= PASSWORD_GLOBAL_LIMIT {
                self.log.ceiling(Ceiling::Password);
                return false;
            }
        }
        for key in keys {
            if !full || addresses.contains_key(&key) {
                addresses.entry(key).or_default().push_back(now);
            }
        }
        true
    }

    /// Only a wrong password spends the global ceiling, so a spray cannot lock out the operator.
    pub fn note_failed_password(&self) {
        let mut state = lock(&self.state);
        let now = Instant::now();
        expire(&mut state.failed_passwords, now);
        state.failed_passwords.push_back(now);
    }
}

fn expire(attempts: &mut Attempts, now: Instant) {
    while attempts.front().is_some_and(|&time| now.duration_since(time) >= WINDOW) {
        attempts.pop_front();
    }
}

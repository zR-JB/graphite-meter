use crate::sync::lock;
use std::{
    fmt::Write,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;

#[derive(Clone, Copy)]
pub(super) enum Counter {
    Local,
    Oidc,
    InvalidPassword,
    OidcFailure,
    GroupDenial,
    ReplayExpiry,
    Throttled,
    Logout,
    CliApproval,
    Capacity,
}

impl Counter {
    pub const COUNT: usize = Self::Capacity as usize + 1;
}

/// Go's ceilingLogInterval: a global ceiling's engagement is logged at most this often.
const CEILING_LOG_INTERVAL: Duration = Duration::from_secs(60);

const COUNTERS: [&str; Counter::COUNT] = [
    "local",
    "oidc",
    "invalid-password",
    "oidc-failure",
    "group-denial",
    "replay-expiry",
    "throttled",
    "logout",
    "cli-approval",
    "capacity",
];

#[derive(Clone, Copy)]
pub(super) enum Ceiling {
    Password,
    PasswordAddress,
    ExchangeAddress,
    StartAddress,
    ApprovalAddress,
    OidcTransaction,
}
const CEILINGS: [&str; Ceiling::OidcTransaction as usize + 1] = [
    "password-attempt",
    "password-attempt-address",
    "oidc-exchange-address",
    "oidc-start-address",
    "browser-approval-address",
    "oidc-transaction",
];

#[derive(Default)]
pub(super) struct SecurityLog {
    counters: [AtomicU64; Counter::COUNT],
    verbose: AtomicBool,
    ceilings: Mutex<[Option<Instant>; CEILINGS.len()]>,
}
impl SecurityLog {
    pub fn configure(&self, verbose: bool) {
        self.verbose.store(verbose, Ordering::Relaxed);
    }
    pub fn count(&self, counter: Counter) {
        self.counters[counter as usize].fetch_add(1, Ordering::Relaxed);
    }
    pub fn debug(&self, message: std::fmt::Arguments<'_>) {
        if self.verbose.load(Ordering::Relaxed) {
            crate::log!("[gm:auth:debug] {message}");
        }
    }
    pub fn refused(&self, reason: super::reason::Reason) {
        use super::reason::Reason;
        self.debug(format_args!("login rejected reason={}", reason.code()));
        self.count(match reason {
            Reason::Throttled => Counter::Throttled,
            Reason::PasswordMismatch => Counter::InvalidPassword,
            Reason::SessionCapacity | Reason::TransactionCapacity => Counter::Capacity,
            Reason::TransactionReplay => Counter::ReplayExpiry,
            Reason::GroupDenied => Counter::GroupDenial,
            _ => return,
        });
    }
    pub fn ceiling(&self, ceiling: Ceiling) {
        let mut ceilings = lock(&self.ceilings);
        let now = Instant::now();
        let last = &mut ceilings[ceiling as usize];
        if last.is_some_and(|last| now.duration_since(last) < CEILING_LOG_INTERVAL) {
            return;
        }
        *last = Some(now);
        drop(ceilings);
        let name = CEILINGS[ceiling as usize];
        crate::log!("[gm:auth] global {name} ceiling engaged; further attempts are refused until the window drains");
    }
    pub fn window(&self, last: &mut [u64; Counter::COUNT]) -> Option<String> {
        let values = self.counters.each_ref().map(|counter| counter.load(Ordering::Relaxed));
        if values == *last {
            return None;
        }
        let mut line = String::from("[gm:auth] 1m");
        for ((name, value), prior) in COUNTERS.iter().zip(values).zip(last.iter_mut()) {
            write!(line, " {name}={}", value.wrapping_sub(*prior)).expect("string writer");
            *prior = value;
        }
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::rate::{AttemptLimiter, Budget};
    use std::sync::Arc;

    #[tokio::test(start_paused = true)]
    async fn password_global_ceiling_is_reported_once_per_draining_window() {
        let log = Arc::new(SecurityLog::default());
        let limiter = AttemptLimiter::with_log(log.clone());
        let address = |last: u8| std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, last));
        for last in 1..=60 {
            assert!(limiter.allow(Budget::Password, address(last)));
            limiter.note_failed_password();
        }
        assert!(!limiter.allow(Budget::Password, address(61)));
        let first = log.ceilings.lock().unwrap()[Ceiling::Password as usize].unwrap();
        tokio::time::advance(Duration::from_secs(59)).await;
        assert!(!limiter.allow(Budget::Password, address(62)));
        assert_eq!(log.ceilings.lock().unwrap()[Ceiling::Password as usize], Some(first));
        tokio::time::advance(Duration::from_secs(1)).await;
        for last in 1..=60 {
            assert!(limiter.allow(Budget::Password, address(last)));
            limiter.note_failed_password();
        }
        assert!(!limiter.allow(Budget::Password, address(63)));
        assert!(log.ceilings.lock().unwrap()[Ceiling::Password as usize].unwrap() > first);
    }
}

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
impl Ceiling {
    fn name(self) -> &'static str {
        match self {
            Self::Password => "password-attempt",
            Self::PasswordAddress => "password-attempt-address",
            Self::ExchangeAddress => "oidc-exchange-address",
            Self::StartAddress => "oidc-start-address",
            Self::ApprovalAddress => "browser-approval-address",
            Self::OidcTransaction => "oidc-transaction",
        }
    }
}

#[derive(Default)]
pub(super) struct SecurityLog {
    counters: [AtomicU64; Counter::COUNT],
    verbose: AtomicBool,
    ceilings: Mutex<[Option<Instant>; Ceiling::OidcTransaction as usize + 1]>,
}
impl SecurityLog {
    pub fn configure(&self, verbose: bool) {
        self.verbose.store(verbose, Ordering::Relaxed);
    }
    pub fn count(&self, counter: Counter) {
        self.counters[counter as usize].fetch_add(1, Ordering::Relaxed);
    }
    pub fn debug(&self, message: &'static str) {
        if self.verbose.load(Ordering::Relaxed) {
            crate::log!("[gm:auth:debug] {message}");
        }
    }
    pub fn ceiling(&self, ceiling: Ceiling) {
        if self.ceiling_due(ceiling) {
            crate::log!(
                "[gm:auth] global {} ceiling engaged; further attempts are refused until the window drains",
                ceiling.name()
            );
        }
    }
    fn ceiling_due(&self, ceiling: Ceiling) -> bool {
        let mut ceilings = self.ceilings.lock().expect("auth log mutex poisoned");
        let now = Instant::now();
        let last = &mut ceilings[ceiling as usize];
        if last.is_some_and(|last| now.duration_since(last) < Duration::from_secs(60)) {
            return false;
        }
        *last = Some(now);
        true
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

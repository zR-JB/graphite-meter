//! The security log: sign-in outcomes counted into one line a minute, and debug lines under `GM_VERBOSE`.

use crate::log;
use std::{
    fmt::{self, Write as _},
    sync::atomic::{AtomicU64, Ordering},
};

/// The counters of the minute line, in its order.
const NAMES: [&str; COUNTERS] = [
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
/// How many counters the minute line reports.
pub const COUNTERS: usize = 10;

/// A counted outcome, at its place in the minute line.
#[derive(Debug, Clone, Copy)]
pub(super) enum Counter {
    Local = 0,
    Oidc = 1,
    InvalidPassword = 2,
    OidcFailure = 3,
    GroupDenial = 4,
    ReplayExpiry = 5,
    Throttled = 6,
    Logout = 7,
    CliApproval = 8,
    Capacity = 9,
}

/// Why a sign-in was refused: Go's reason codes, and the notice the sign-in page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reason {
    CsrfOriginMissing,
    CsrfOriginMismatch,
    CsrfCookieMissing,
    CsrfTokenMissing,
    CsrfTokenMismatch,
    MalformedForm,
    Throttled,
    VerifierBusy,
    PasswordMismatch,
    SessionCapacity,
    ProviderNotReady,
    TransactionCapacity,
    ExchangeRateLimited,
    CallbackParameters,
    TransactionCookie,
    TransactionReplay,
    ResponseIssuer,
    TokenExchange,
    MissingIdToken,
    IdTokenVerification,
    IdTokenClaimsOrNonce,
    AccessTokenHash,
    UserInfoOrSubject,
    UserInfoClaims,
    /// No allowed group: reported as `userinfo_claims_or_group`, counted apart.
    GroupDenied,
    InvalidSubject,
}

impl Reason {
    fn code(self) -> &'static str {
        match self {
            Self::CsrfOriginMissing => "csrf_origin_missing",
            Self::CsrfOriginMismatch => "csrf_origin_mismatch",
            Self::CsrfCookieMissing => "csrf_cookie_missing",
            Self::CsrfTokenMissing => "csrf_token_missing",
            Self::CsrfTokenMismatch => "csrf_token_mismatch",
            Self::MalformedForm => "malformed_form",
            Self::Throttled => "rate_limited_or_client_address",
            Self::VerifierBusy => "verifier_busy",
            Self::PasswordMismatch => "password_mismatch",
            Self::SessionCapacity => "session_capacity",
            Self::ProviderNotReady => "provider_not_ready",
            Self::TransactionCapacity => "transaction_capacity",
            Self::ExchangeRateLimited => "exchange_rate_limited",
            Self::CallbackParameters => "callback_parameters",
            Self::TransactionCookie => "transaction_cookie",
            Self::TransactionReplay => "transaction_replay_or_expiry",
            Self::ResponseIssuer => "response_issuer",
            Self::TokenExchange => "token_exchange",
            Self::MissingIdToken => "missing_id_token",
            Self::IdTokenVerification => "id_token_verification",
            Self::IdTokenClaimsOrNonce => "id_token_claims_or_nonce",
            Self::AccessTokenHash => "access_token_hash",
            Self::UserInfoOrSubject => "userinfo_or_subject",
            Self::UserInfoClaims | Self::GroupDenied => "userinfo_claims_or_group",
            Self::InvalidSubject => "invalid_subject",
        }
    }

    pub fn notice(self) -> &'static str {
        match self {
            Self::ProviderNotReady => "provider",
            Self::VerifierBusy | Self::SessionCapacity | Self::TransactionCapacity => "busy",
            Self::Throttled => "throttled",
            Self::PasswordMismatch => "password",
            Self::CsrfCookieMissing | Self::CsrfTokenMissing | Self::TransactionCookie => "stale",
            _ => "failed",
        }
    }
}

/// The counts since start and whether debug lines are written.
pub struct Security {
    counts: [AtomicU64; COUNTERS],
    verbose: bool,
}

impl Security {
    pub(super) fn new(verbose: bool) -> Self {
        Self { counts: Default::default(), verbose }
    }

    pub(super) fn count(&self, counter: Counter) {
        self.counts[counter as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn debug(&self, message: fmt::Arguments<'_>) {
        if self.verbose {
            log!("[gm:auth:debug] {message}");
        }
    }

    /// Logs a refused sign-in and counts the refusals the minute line reports.
    pub(super) fn refused(&self, reason: Reason) {
        self.debug(format_args!("login rejected reason={}", reason.code()));
        match reason {
            Reason::Throttled => self.count(Counter::Throttled),
            Reason::PasswordMismatch => self.count(Counter::InvalidPassword),
            Reason::SessionCapacity | Reason::TransactionCapacity => self.count(Counter::Capacity),
            Reason::TransactionReplay => self.count(Counter::ReplayExpiry),
            Reason::GroupDenied => self.count(Counter::GroupDenial),
            _ => {}
        }
    }

    /// The minute line of the counts since `last`, which it advances; `None` when nothing changed.
    pub fn line(&self, last: &mut [u64; COUNTERS]) -> Option<String> {
        let counts = self.counts.each_ref().map(|count| count.load(Ordering::Relaxed));
        if counts == *last {
            return None;
        }
        let mut line = String::from("[gm:auth] 1m");
        for ((name, count), last) in NAMES.iter().zip(counts).zip(last.iter_mut()) {
            write!(line, " {name}={}", count - *last).expect("strings take writes");
            *last = count;
        }
        Some(line)
    }
}

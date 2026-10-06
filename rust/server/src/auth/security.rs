//! The security log: sign-in outcomes counted into one line a minute, and debug lines under `GM_VERBOSE`.

use crate::log;
use std::{
    fmt::{self, Write as _},
    sync::atomic::{AtomicU64, Ordering},
};

graphite_meter_proto::table! {
    /// A counted outcome, at its place in the minute line.
    pub(super) enum Counter {
        name.0: &'static str,
    } {
        Local => ("local",),
        Oidc => ("oidc",),
        InvalidPassword => ("invalid-password",),
        OidcFailure => ("oidc-failure",),
        GroupDenial => ("group-denial",),
        ReplayExpiry => ("replay-expiry",),
        Throttled => ("throttled",),
        Logout => ("logout",),
        CliApproval => ("cli-approval",),
        Capacity => ("capacity",),
    }
}

/// How many counters the minute line reports.
pub const COUNTERS: usize = Counter::ALL.len();

graphite_meter_proto::table! {
    /// Why a sign-in was refused: its security log code, the sign-in page's notice and the counter it adds to.
    pub(super) enum Reason {
        code.0: &'static str,
        notice.1: &'static str,
        counter.2: Option<Counter>,
    } {
        CsrfOriginMissing => ("csrf_origin_missing", "failed", None),
        CsrfOriginMismatch => ("csrf_origin_mismatch", "failed", None),
        CsrfCookieMissing => ("csrf_cookie_missing", "stale", None),
        CsrfTokenMissing => ("csrf_token_missing", "stale", None),
        CsrfTokenMismatch => ("csrf_token_mismatch", "failed", None),
        MalformedForm => ("malformed_form", "failed", None),
        Throttled => ("rate_limited_or_client_address", "throttled", Some(Counter::Throttled)),
        VerifierBusy => ("verifier_busy", "busy", None),
        PasswordMismatch => ("password_mismatch", "password", Some(Counter::InvalidPassword)),
        SessionCapacity => ("session_capacity", "busy", Some(Counter::Capacity)),
        ProviderNotReady => ("provider_not_ready", "provider", None),
        TransactionCapacity => ("transaction_capacity", "busy", Some(Counter::Capacity)),
        ExchangeRateLimited => ("exchange_rate_limited", "failed", None),
        CallbackParameters => ("callback_parameters", "failed", None),
        TransactionCookie => ("transaction_cookie", "stale", None),
        TransactionReplay => ("transaction_replay_or_expiry", "failed", Some(Counter::ReplayExpiry)),
        ResponseIssuer => ("response_issuer", "failed", None),
        TokenExchange => ("token_exchange", "failed", None),
        MissingIdToken => ("missing_id_token", "failed", None),
        IdTokenVerification => ("id_token_verification", "failed", None),
        IdTokenClaimsOrNonce => ("id_token_claims_or_nonce", "failed", None),
        AccessTokenHash => ("access_token_hash", "failed", None),
        UserInfoOrSubject => ("userinfo_or_subject", "failed", None),
        UserInfoClaims => ("userinfo_claims_or_group", "failed", None),
        /// No allowed group: reported as `userinfo_claims_or_group`, counted apart.
        GroupDenied => ("userinfo_claims_or_group", "failed", Some(Counter::GroupDenial)),
        InvalidSubject => ("invalid_subject", "failed", None),
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
        if let Some(counter) = reason.counter() {
            self.count(counter);
        }
    }

    /// The minute line of the counts since `last`, which it advances; `None` when nothing changed.
    pub fn line(&self, last: &mut [u64; COUNTERS]) -> Option<String> {
        let counts = self.counts.each_ref().map(|count| count.load(Ordering::Relaxed));
        if counts == *last {
            return None;
        }
        let mut line = String::from("[gm:auth] 1m");
        for ((counter, count), last) in Counter::ALL.iter().zip(counts).zip(last.iter_mut()) {
            write!(line, " {}={}", counter.name(), count - *last).expect("strings take writes");
            *last = count;
        }
        Some(line)
    }
}

//! Go's sign-in refusal reasons: logged when verbose, each shows one notice on the sign-in page.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
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
    /// Logged as `UserInfoClaims` but counted apart, as in Go.
    GroupDenied,
    InvalidSubject,
}

impl Reason {
    pub const fn code(self) -> &'static str {
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

    pub const fn notice(self) -> &'static str {
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

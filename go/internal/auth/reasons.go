package auth

// reasons.go is the single classification of why a sign-in attempt failed.
// One reason value feeds three outputs — the verbose debug log, the counters,
// and the message the login page renders — so a reason can never be logged
// under one name and counted under another.
//
// Only the reasons listed in reasonNotices produce a distinct page message.
// Everything else, and in particular every credential outcome, collapses to
// noticeGeneric: the page must not tell a wrong password from an unknown one,
// a throttled address from an untried one, or a denied group from a denied
// signature.

type reason string

const (
	// Shared login-form CSRF outcomes.
	reasonCSRFOriginMissing  reason = "csrf_origin_missing"
	reasonCSRFOriginMismatch reason = "csrf_origin_mismatch"
	reasonCSRFCookieMissing  reason = "csrf_cookie_missing"
	reasonCSRFTokenMissing   reason = "csrf_token_missing"
	reasonCSRFTokenMismatch  reason = "csrf_token_mismatch"

	// Operator-password outcomes.
	reasonFormMalformed    reason = "malformed_form"
	reasonThrottled        reason = "rate_limited_or_client_address"
	reasonVerifierBusy     reason = "verifier_busy"
	reasonPasswordMismatch reason = "password_mismatch"
	reasonSessionCapacity  reason = "session_capacity"

	// OIDC authorization-request outcomes.
	reasonProviderNotReady      reason = "provider_not_ready"
	reasonStateGeneration       reason = "state_generation"
	reasonNonceGeneration       reason = "nonce_generation"
	reasonBrowserBinding        reason = "browser_binding_generation"
	reasonClientAddress         reason = "client_address"
	reasonTransactionCapacity   reason = "transaction_capacity"
	reasonExchangeRateLimited   reason = "exchange_rate_limited"
	reasonCallbackParameters    reason = "callback_parameters"
	reasonTransactionCookie     reason = "transaction_cookie"
	reasonTransactionReplay     reason = "transaction_replay_or_expiry"
	reasonResponseIssuer        reason = "response_issuer"
	reasonTokenExchange         reason = "token_exchange"
	reasonMissingIDToken        reason = "missing_id_token"
	reasonIDTokenVerification   reason = "id_token_verification"
	reasonIDTokenClaimsOrNonce  reason = "id_token_claims_or_nonce"
	reasonAccessTokenHash       reason = "access_token_hash"
	reasonUserInfoOrSubject     reason = "userinfo_or_subject"
	reasonUserInfoClaimsOrGroup reason = "userinfo_claims_or_group"
	reasonInvalidSubject        reason = "invalid_subject"
)

// notice is the stable, non-secret code carried in the login page's `error`
// query parameter. The wording lives in the template; only these four codes
// ever cross the wire.
type notice string

const (
	noticeGeneric  notice = "failed"
	noticeProvider notice = "provider"
	noticeBusy     notice = "busy"
	noticeStale    notice = "stale"
)

// reasonNotices is the safe subset: reasons a visitor may be told apart. Each
// describes the state of the server or of the visitor's own form — never the
// outcome of a credential check.
var reasonNotices = map[reason]notice{
	reasonProviderNotReady:    noticeProvider,
	reasonVerifierBusy:        noticeBusy,
	reasonSessionCapacity:     noticeBusy,
	reasonTransactionCapacity: noticeBusy,
	reasonCSRFCookieMissing:   noticeStale,
	reasonCSRFTokenMissing:    noticeStale,
	reasonTransactionCookie:   noticeStale,
}

func noticeFor(why reason) notice {
	if n, ok := reasonNotices[why]; ok {
		return n
	}
	return noticeGeneric
}

// parseNotice accepts only the four known codes, so nothing from the query
// string reaches the template unvalidated.
func parseNotice(raw string) notice {
	switch notice(raw) {
	case noticeProvider:
		return noticeProvider
	case noticeBusy:
		return noticeBusy
	case noticeStale:
		return noticeStale
	case "":
		return ""
	}
	return noticeGeneric
}

// counterID names the counter a reason is charged to, if any.
type counterID int

const (
	counterNone counterID = iota
	counterThrottled
	counterInvalidPassword
)

// reasonCounters charges a reason to a counter. Capacity, group-denial, and
// replay counters are bumped where the condition is detected, because they
// also cover paths that never reach a login response.
var reasonCounters = map[reason]counterID{
	reasonThrottled:        counterThrottled,
	reasonPasswordMismatch: counterInvalidPassword,
}

func (s *Service) countReason(why reason) {
	switch reasonCounters[why] {
	case counterThrottled:
		s.counters.throttled.Add(1)
	case counterInvalidPassword:
		s.counters.invalidPassword.Add(1)
	case counterNone:
	}
}

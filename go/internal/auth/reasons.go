package auth

// reasons.go is the single classification of why a sign-in attempt failed. One
// reason value feeds the verbose debug log, the counters, and the message the
// login page renders, so a reason cannot be logged under one name and counted
// under another. Only the reasons in reasonNotices reach the visitor.

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
// query parameter. The wording lives in the template; only the codes below ever
// cross the wire.
type notice string

const (
	noticeGeneric   notice = "failed"
	noticeProvider  notice = "provider"
	noticeBusy      notice = "busy"
	noticeStale     notice = "stale"
	noticeThrottled notice = "throttled"
	noticePassword  notice = "password"
)

// reasonNotices is the safe subset: reasons a visitor may tell apart. A wrong
// operator password and rate limiting say so plainly, because the source is
// public and the real defenses are the attempt budget and the memory-hard
// hash, not vague wording.
var reasonNotices = map[reason]notice{
	reasonProviderNotReady:    noticeProvider,
	reasonVerifierBusy:        noticeBusy,
	reasonSessionCapacity:     noticeBusy,
	reasonTransactionCapacity: noticeBusy,
	reasonThrottled:           noticeThrottled,
	reasonPasswordMismatch:    noticePassword,
	reasonCSRFCookieMissing:   noticeStale,
	reasonCSRFTokenMissing:    noticeStale,
	reasonTransactionCookie:   noticeStale,
}

// noticeFor maps a reason to the code the visitor sees, defaulting to the
// generic one. The OIDC identity outcomes (group, subject, signature) take that
// default: telling them apart reveals whether a named person is authorized here.
func noticeFor(why reason) notice {
	if n, ok := reasonNotices[why]; ok {
		return n
	}
	return noticeGeneric
}

// parseNotice accepts only the known codes, collapsing anything else to the
// generic one, so nothing from the query string reaches the template
// unvalidated.
func parseNotice(raw string) notice {
	switch notice(raw) {
	case noticeProvider:
		return noticeProvider
	case noticeBusy:
		return noticeBusy
	case noticeStale:
		return noticeStale
	case noticeThrottled:
		return noticeThrottled
	case noticePassword:
		return noticePassword
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

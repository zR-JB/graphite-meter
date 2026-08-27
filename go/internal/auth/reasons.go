package auth

type reason string

const (
	reasonCSRFOriginMissing  reason = "csrf_origin_missing"
	reasonCSRFOriginMismatch reason = "csrf_origin_mismatch"
	reasonCSRFCookieMissing  reason = "csrf_cookie_missing"
	reasonCSRFTokenMissing   reason = "csrf_token_missing"
	reasonCSRFTokenMismatch  reason = "csrf_token_mismatch"

	reasonFormMalformed    reason = "malformed_form"
	reasonThrottled        reason = "rate_limited_or_client_address"
	reasonVerifierBusy     reason = "verifier_busy"
	reasonPasswordMismatch reason = "password_mismatch"
	reasonSessionCapacity  reason = "session_capacity"

	reasonProviderNotReady      reason = "provider_not_ready"
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

type notice string

const (
	noticeGeneric   notice = "failed"
	noticeProvider  notice = "provider"
	noticeBusy      notice = "busy"
	noticeStale     notice = "stale"
	noticeThrottled notice = "throttled"
	noticePassword  notice = "password"
)

func noticeFor(why reason) notice {
	switch why {
	case reasonProviderNotReady:
		return noticeProvider
	case reasonVerifierBusy, reasonSessionCapacity, reasonTransactionCapacity:
		return noticeBusy
	case reasonThrottled:
		return noticeThrottled
	case reasonPasswordMismatch:
		return noticePassword
	case reasonCSRFCookieMissing, reasonCSRFTokenMissing, reasonTransactionCookie:
		return noticeStale
	}
	return noticeGeneric
}

func parseNotice(raw string) notice {
	n := notice(raw)
	if n == "" || n == noticeProvider || n == noticeBusy || n == noticeStale || n == noticeThrottled || n == noticePassword {
		return n
	}
	return noticeGeneric
}

func (s *Service) countReason(why reason) {
	switch why {
	case reasonThrottled:
		s.counters.throttled.Add(1)
	case reasonPasswordMismatch:
		s.counters.invalidPassword.Add(1)
	}
}

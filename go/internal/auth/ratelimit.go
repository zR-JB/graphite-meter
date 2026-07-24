package auth

// ratelimit.go holds the attempt budgets. Every budget is bounded in memory,
// which trades availability for a store an anonymous caller cannot grow: a
// determined attacker can hold a global ceiling engaged, but cannot make the
// process allocate without limit. noteCeilingLocked exists so an operator can
// tell those two states apart from the log.

import (
	"log"
	"net/http"
	"net/netip"
	"time"
)

const (
	// attemptWindow is the width of every budget below.
	attemptWindow = time.Minute
	// maxBudgetKeys bounds how many distinct addresses a store tracks.
	maxBudgetKeys = 2048
	// maxAddressAttempts is the password attempts one address may spend.
	maxAddressAttempts = 5
	// maxGlobalAttempts is the password attempts all addresses may spend.
	maxGlobalAttempts = 60
	// maxAddressExchanges is the OIDC token exchanges one address may spend.
	// Each exchange is one outbound request to the identity provider, so this
	// is the only lever bounding attacker-driven traffic toward the IdP.
	maxAddressExchanges = 10
	// ceilingLogInterval throttles the operator notice, so holding a ceiling
	// engaged cannot also be used to flood the log.
	ceilingLogInterval = time.Minute
)

type loginAttempt struct {
	times []time.Time
}

// budgetKey collapses an address to the unit its budget is charged to: the
// full address for IPv4, and the /64 for IPv6. A single IPv6 allocation
// routinely hands one host 2^64 addresses, so keying per /128 would let one
// customer prefix sidestep every per-address limit here.
func budgetKey(addr netip.Addr) string {
	addr = addr.Unmap()
	if addr.Is6() {
		if p, err := addr.Prefix(64); err == nil {
			return p.String()
		}
	}
	return addr.String()
}

// attemptRoomLocked reports the surviving attempts for key and whether one
// more fits. It sweeps the store when the key count is at its ceiling, and
// fails closed when sweeping does not free room. It records nothing: the
// caller commits only after every other budget has also cleared.
func (s *Service) attemptRoomLocked(store map[string]loginAttempt, name, key string, limit int, now time.Time) ([]time.Time, bool) {
	if _, exists := store[key]; !exists && len(store) >= maxBudgetKeys {
		for k, v := range store {
			v.times = recentAttempts(v.times, now)
			if len(v.times) == 0 {
				delete(store, k)
			} else {
				store[k] = v
			}
		}
		if len(store) >= maxBudgetKeys {
			s.noteCeilingLocked(name+"-address", now)
			return nil, false
		}
	}
	times := recentAttempts(store[key].times, now)
	if len(times) >= limit {
		return nil, false
	}
	return times, true
}

// allowAttempt charges one password attempt to the request's client address.
// It fails closed when the address cannot be determined, when the address has
// spent its budget, or when the global ceiling is engaged.
func (s *Service) allowAttempt(r *http.Request) bool {
	addr, ok := s.authClientAddress(r)
	if !ok {
		return false
	}
	key := budgetKey(addr)
	now := s.now()
	s.mu.Lock()
	defer s.mu.Unlock()
	times, ok := s.attemptRoomLocked(s.attempts, "password-attempt", key, maxAddressAttempts, now)
	if !ok {
		return false
	}
	s.globalAttempts = recentAttempts(s.globalAttempts, now)
	if len(s.globalAttempts) >= maxGlobalAttempts {
		s.noteCeilingLocked("password-attempt", now)
		return false
	}
	s.attempts[key] = loginAttempt{times: append(times, now)}
	s.globalAttempts = append(s.globalAttempts, now)
	return true
}

// allowExchange charges one OIDC token exchange to the request's client
// address. An attacker can mint their own transaction and reach the exchange
// with an arbitrary code, which makes the exchange the one path that turns an
// anonymous request into an outbound request to the identity provider — often
// on a private network the deployment is otherwise keeping traffic out of.
func (s *Service) allowExchange(r *http.Request) bool {
	addr, ok := s.authClientAddress(r)
	if !ok {
		return false
	}
	key := budgetKey(addr)
	now := s.now()
	s.mu.Lock()
	defer s.mu.Unlock()
	times, ok := s.attemptRoomLocked(s.exchanges, "oidc-exchange", key, maxAddressExchanges, now)
	if !ok {
		return false
	}
	s.exchanges[key] = loginAttempt{times: append(times, now)}
	return true
}

// noteCeilingLocked logs at most once per interval that a global bound is
// refusing work. Without it a saturated ceiling is indistinguishable from a
// broken service in the operator's log.
func (s *Service) noteCeilingLocked(what string, now time.Time) {
	if last, ok := s.ceilingLogged[what]; ok && now.Sub(last) < ceilingLogInterval {
		return
	}
	s.ceilingLogged[what] = now
	log.Printf("[gm:auth] global %s ceiling engaged; further attempts are refused until the window drains", what)
}

// noteCeiling is noteCeilingLocked for callers that do not already hold the
// service lock.
func (s *Service) noteCeiling(what string, now time.Time) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.noteCeilingLocked(what, now)
}

// recentAttempts drops everything older than the attempt window.
func recentAttempts(attempts []time.Time, now time.Time) []time.Time {
	cutoff := now.Add(-attemptWindow)
	start := 0
	for start < len(attempts) && !attempts[start].After(cutoff) {
		start++
	}
	return attempts[start:]
}

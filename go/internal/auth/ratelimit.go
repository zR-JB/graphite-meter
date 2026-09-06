package auth

import (
	"log"
	"maps"
	"net/http"
	"net/netip"
	"time"
)

const (
	attemptWindow       = time.Minute
	maxBudgetKeys       = 2048
	maxAddressAttempts  = 5
	maxGlobalAttempts   = 60
	maxAddressExchanges = 10
	maxAddressApprovals = 10
	ceilingLogInterval  = time.Minute
)

type loginAttempt struct {
	times []time.Time
}

func budgetKey(addr netip.Addr) string {
	addr = addr.Unmap()
	if addr.Is6() {
		if p, err := addr.Prefix(64); err == nil {
			return p.String()
		}
	}
	return addr.String()
}

func (s *Service) attemptRoomLocked(store map[string]loginAttempt, name, key string, limit int, now time.Time) ([]time.Time, bool) {
	if _, exists := store[key]; !exists && len(store) >= maxBudgetKeys {
		maps.DeleteFunc(store, func(k string, v loginAttempt) bool {
			v.times = recentAttempts(v.times, now)
			if len(v.times) == 0 {
				return true
			}
			store[k] = v
			return false
		})
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

func (s *Service) allowAddress(r *http.Request, store map[string]loginAttempt, name string, limit int, commit func(string, []time.Time, time.Time) bool) bool {
	addr, ok := s.authClientAddress(r)
	if !ok {
		return false
	}
	key := budgetKey(addr)
	now := s.now()
	s.mu.Lock()
	defer s.mu.Unlock()
	times, ok := s.attemptRoomLocked(store, name, key, limit, now)
	if !ok {
		return false
	}
	return commit(key, times, now)
}

func (s *Service) allowAttempt(r *http.Request) bool {
	return s.allowAddress(r, s.attempts, "password-attempt", maxAddressAttempts, func(key string, times []time.Time, now time.Time) bool {
		s.globalAttempts = recentAttempts(s.globalAttempts, now)
		if len(s.globalAttempts) >= maxGlobalAttempts {
			s.noteCeilingLocked("password-attempt", now)
			return false
		}
		s.attempts[key] = loginAttempt{times: append(times, now)}
		s.globalAttempts = append(s.globalAttempts, now)
		return true
	})
}

func (s *Service) allowExchange(r *http.Request) bool {
	return s.allowAddress(r, s.exchanges, "oidc-exchange", maxAddressExchanges, func(key string, times []time.Time, now time.Time) bool {
		s.exchanges[key] = loginAttempt{times: append(times, now)}
		return true
	})
}

// Approval pages are public; their callers cannot spend validated OIDC callbacks' budget.
func (s *Service) allowBrowserApproval(r *http.Request) bool {
	return s.allowAddress(r, s.approvalAttempts, "browser-approval", maxAddressApprovals, func(key string, times []time.Time, now time.Time) bool {
		s.approvalAttempts[key] = loginAttempt{times: append(times, now)}
		return true
	})
}

func (s *Service) noteCeilingLocked(what string, now time.Time) {
	if last, ok := s.ceilingLogged[what]; ok && now.Sub(last) < ceilingLogInterval {
		return
	}
	s.ceilingLogged[what] = now
	log.Printf("[gm:auth] global %s ceiling engaged; further attempts are refused until the window drains", what)
}

func recentAttempts(attempts []time.Time, now time.Time) []time.Time {
	cutoff := now.Add(-attemptWindow)
	start := 0
	for start < len(attempts) && !attempts[start].After(cutoff) {
		start++
	}
	return attempts[start:]
}

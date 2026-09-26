package auth

import (
	"log"
	"maps"
	"net/http"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
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

func (s *Service) allowAddress(r *http.Request, store map[string][]time.Time, name string, limit int,
	global *[]time.Time) bool {
	addr, ok := s.authClientAddress(r)
	if !ok {
		return false
	}
	key := transport.AddressBucket(addr)
	now := time.Now()
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, exists := store[key]; !exists && len(store) >= maxBudgetKeys {
		maps.DeleteFunc(store, func(k string, times []time.Time) bool {
			store[k] = recentAttempts(times, now)
			return len(store[k]) == 0
		})
		if len(store) >= maxBudgetKeys {
			s.noteCeilingLocked(name+"-address", now)
			return false
		}
	}
	times := recentAttempts(store[key], now)
	if len(times) >= limit {
		return false
	}
	if global != nil {
		*global = recentAttempts(*global, now)
		if len(*global) >= maxGlobalAttempts {
			s.noteCeilingLocked(name, now)
			return false
		}
		*global = append(*global, now)
	}
	store[key] = append(times, now)
	return true
}

func (s *Service) allowAttempt(r *http.Request) bool {
	return s.allowAddress(r, s.attempts, "password-attempt", maxAddressAttempts, &s.globalAttempts)
}

func (s *Service) allowExchange(r *http.Request) bool {
	return s.allowAddress(r, s.exchanges, "oidc-exchange", maxAddressExchanges, nil)
}

// Approval pages are public; their callers cannot spend validated OIDC callbacks' budget.
func (s *Service) allowBrowserApproval(r *http.Request) bool {
	return s.allowAddress(r, s.approvalAttempts, "browser-approval", maxAddressApprovals, nil)
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

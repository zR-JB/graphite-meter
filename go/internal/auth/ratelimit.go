package auth

// ratelimit.go holds the attempt budgets. Both the per-address and the global
// ceiling are bounded in memory, which trades availability for a store that
// cannot be grown by an anonymous caller.

import (
	"net/http"
	"time"
)

type loginAttempt struct {
	times []time.Time
}

// allowAttempt charges one password attempt to the request's client address.
// It fails closed when the address cannot be determined, when the address has
// spent its per-minute budget, or when the global ceiling is engaged.
func (s *Service) allowAttempt(r *http.Request) bool {
	addr, ok := s.authClientAddress(r)
	if !ok {
		return false
	}
	key := addr.String()
	now := s.now()
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, exists := s.attempts[key]; !exists && len(s.attempts) >= 2048 {
		for k, v := range s.attempts {
			v.times = recentAttempts(v.times, now)
			if len(v.times) == 0 {
				delete(s.attempts, k)
			} else {
				s.attempts[k] = v
			}
		}
		if len(s.attempts) >= 2048 {
			return false
		}
	}
	a := s.attempts[key]
	a.times = recentAttempts(a.times, now)
	if len(a.times) >= 5 {
		return false
	}
	s.globalAttempts = recentAttempts(s.globalAttempts, now)
	if len(s.globalAttempts) >= 60 {
		return false
	}
	a.times = append(a.times, now)
	s.attempts[key] = a
	s.globalAttempts = append(s.globalAttempts, now)
	return true
}

// recentAttempts drops everything older than the one-minute window.
func recentAttempts(attempts []time.Time, now time.Time) []time.Time {
	cutoff := now.Add(-time.Minute)
	first := 0
	for first < len(attempts) && !attempts[first].After(cutoff) {
		first++
	}
	return attempts[first:]
}

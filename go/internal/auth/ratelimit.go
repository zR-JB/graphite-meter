package auth

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"log"
	"maps"
	"net/http"
	"time"
)

const (
	attemptWindow        = time.Minute
	maxBudgetKeys        = 2048
	maxAddressAttempts   = 5
	maxGlobalAttempts    = 60
	maxAddressExchanges  = 10
	maxAddressOIDCStarts = 10
	maxAddressApprovals  = 10
	ceilingLogInterval   = time.Minute
	deviceLifetime       = 30 * 24 * time.Hour
)

func (s *Service) allowAddress(r *http.Request, store map[string][]time.Time, name string, limit int,
	global *[]time.Time) bool {
	keys, ok := ClientKeys(r, s.trusted)
	if !ok {
		return false
	}
	now := time.Now()
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(store)+len(keys) > maxBudgetKeys {
		maps.DeleteFunc(store, func(k string, times []time.Time) bool {
			store[k] = recentAttempts(times, now)
			return len(store[k]) == 0
		})
	}
	for i, key := range keys {
		if _, exists := store[key]; !exists && len(store) >= maxBudgetKeys {
			s.noteCeilingLocked(name+"-address", now)
			return false
		}
		if store[key] = recentAttempts(store[key], now); len(store[key]) >= limit<<i {
			return false
		}
	}
	if global != nil {
		if *global = recentAttempts(*global, now); len(*global) >= maxGlobalAttempts {
			s.noteCeilingLocked(name, now)
			return false
		}
	}
	for _, key := range keys {
		store[key] = append(store[key], now)
	}
	return true
}

// A browser that signed in before skips the global ceiling, so others' wrong passwords cannot lock the operator out.
func (s *Service) allowAttempt(r *http.Request) bool {
	global := &s.globalAttempts
	if s.knownDevice(r) {
		global = nil
	}
	return s.allowAddress(r, s.attempts, "password-attempt", maxAddressAttempts, global)
}

func (s *Service) noteFailedPassword() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.globalAttempts = append(recentAttempts(s.globalAttempts, time.Now()), time.Now())
}

// The key is the password hash, so a device survives restarts and is forgotten when the password changes.
func (s *Service) deviceTag(expires int64) []byte {
	mac := hmac.New(sha256.New, []byte(s.passwordHash))
	mac.Write(binary.BigEndian.AppendUint64(nil, uint64(expires)))
	return mac.Sum(nil)
}

func (s *Service) issueDeviceCookie(w http.ResponseWriter) {
	expires := time.Now().Add(deviceLifetime)
	value := binary.BigEndian.AppendUint64(nil, uint64(expires.Unix()))
	value = append(value, s.deviceTag(expires.Unix())...)
	setCookie(w, deviceCookie, base64.RawURLEncoding.EncodeToString(value), expires, http.SameSiteStrictMode)
}

func (s *Service) knownDevice(r *http.Request) bool {
	c := uniqueCookie(r, deviceCookie)
	if c == nil {
		return false
	}
	raw, err := base64.RawURLEncoding.DecodeString(c.Value)
	if err != nil || len(raw) != 8+sha256.Size {
		return false
	}
	expires := int64(binary.BigEndian.Uint64(raw))
	return time.Now().Unix() < expires && hmac.Equal(raw[8:], s.deviceTag(expires))
}

func (s *Service) allowExchange(r *http.Request) bool {
	return s.allowAddress(r, s.exchanges, "oidc-exchange", maxAddressExchanges, nil)
}

func (s *Service) allowOIDCStart(r *http.Request) bool {
	return s.allowAddress(r, s.oidcStarts, "oidc-start", maxAddressOIDCStarts, nil)
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

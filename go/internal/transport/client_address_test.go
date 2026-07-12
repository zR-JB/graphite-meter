package transport

import (
	"net/http/httptest"
	"net/netip"
	"testing"
)

func TestResolveClientAddress(t *testing.T) {
	trusted := []netip.Prefix{
		netip.MustParsePrefix("10.0.0.0/8"),
		netip.MustParsePrefix("192.0.2.0/24"),
		netip.MustParsePrefix("::1/128"),
	}
	tests := []struct {
		name, remote, header, value, want, source string
	}{
		{"direct IPv4", "198.51.100.9:1234", "", "", "198.51.100.9", "socket"},
		{"direct IPv6", "[2001:db8::9]:1234", "", "", "2001:db8::9", "socket"},
		{"untrusted spoof ignored", "198.51.100.9:1234", "X-Forwarded-For", "203.0.113.4", "198.51.100.9", "socket"},
		{"Forwarded IPv4 and port", "10.0.0.2:1234", "Forwarded", `for="203.0.113.4:4567";proto=https`, "203.0.113.4", "forwarded"},
		{"Forwarded quoted IPv6", "10.0.0.2:1234", "Forwarded", `for="[2001:db8::4]:4567"`, "2001:db8::4", "forwarded"},
		{"XFF bracketed IPv6", "10.0.0.2:1234", "X-Forwarded-For", "[2001:db8::4]:4567", "2001:db8::4", "forwarded"},
		{"X Real IP", "10.0.0.2:1234", "X-Real-IP", "203.0.113.4", "203.0.113.4", "forwarded"},
		{"Forwarded multi hop", "10.0.0.2:1234", "Forwarded", "for=203.0.113.4;proto=https, for=192.0.2.7", "203.0.113.4", "forwarded"},
		{"multi hop boundary", "10.0.0.2:1234", "X-Forwarded-For", "203.0.113.4, 198.51.100.8, 192.0.2.7", "198.51.100.8", "forwarded"},
		{"all forwarded hops trusted", "10.0.0.2:1234", "X-Forwarded-For", "192.0.2.3, 192.0.2.7", "192.0.2.3", "forwarded"},
		{"mapped peer and client", "[::ffff:10.0.0.2]:1234", "X-Forwarded-For", "::ffff:203.0.113.4", "203.0.113.4", "forwarded"},
		{"malformed header falls back", "10.0.0.2:1234", "X-Forwarded-For", "unknown", "10.0.0.2", "socket"},
		{"obfuscated Forwarded falls back", "10.0.0.2:1234", "Forwarded", "for=_hidden", "10.0.0.2", "socket"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := httptest.NewRequest("GET", "/", nil)
			r.RemoteAddr = tt.remote
			if tt.header != "" {
				r.Header.Set(tt.header, tt.value)
			}
			got := ResolveClientAddress(r, trusted)
			if got.Addr.String() != tt.want || string(got.Source) != tt.source {
				t.Fatalf("ResolveClientAddress() = %s/%s, want %s/%s", got.Addr, got.Source, tt.want, tt.source)
			}
		})
	}
}

func TestForwardedHeaderPrecedence(t *testing.T) {
	r := httptest.NewRequest("GET", "/", nil)
	r.RemoteAddr = "10.0.0.2:1234"
	r.Header.Set("Forwarded", "for=203.0.113.4")
	r.Header.Set("X-Forwarded-For", "198.51.100.8")
	r.Header.Set("X-Real-IP", "198.51.100.9")
	got := ResolveClientAddress(r, []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8")})
	if got.Addr.String() != "203.0.113.4" || got.Source != ClientIPForwarded {
		t.Fatalf("ResolveClientAddress() = %s/%s, want Forwarded value", got.Addr, got.Source)
	}
}

func TestForwardedPrecedenceDoesNotMixMalformedHeaders(t *testing.T) {
	r := httptest.NewRequest("GET", "/", nil)
	r.RemoteAddr = "10.0.0.2:1234"
	r.Header.Set("Forwarded", "for=unknown")
	r.Header.Set("X-Forwarded-For", "203.0.113.4")
	got := ResolveClientAddress(r, []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8")})
	if got.Addr.String() != "10.0.0.2" || got.Source != ClientIPSocket {
		t.Fatalf("ResolveClientAddress() = %s/%s, want socket fallback", got.Addr, got.Source)
	}
}

package transport

import (
	"net/http/httptest"
	"net/netip"
	"strings"
	"testing"
)

func TestResolveClientAddress(t *testing.T) {
	trusted := []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8"), netip.MustParsePrefix("::1/128")}
	for _, tc := range []struct {
		name, remote string
		headers      map[string][]string
		want         string
		source       ClientIPSource
		ok           bool
	}{
		{"direct IPv4", "198.51.100.9:1234", nil, "198.51.100.9", ClientIPSocket, true},
		{"direct IPv6", "[2001:db8::9]:1234", nil, "2001:db8::9", ClientIPSocket, true},
		{"untrusted peer's header ignored", "198.51.100.9:1234",
			map[string][]string{"X-Real-IP": {"203.0.113.4"}}, "198.51.100.9", ClientIPSocket, true},
		{"trusted peer's X-Real-IP", "10.0.0.2:1234",
			map[string][]string{"X-Real-IP": {" 203.0.113.4 "}}, "203.0.113.4", ClientIPForwarded, true},
		{"mapped peer and client", "[::ffff:10.0.0.2]:1234",
			map[string][]string{"X-Real-IP": {"::ffff:203.0.113.4"}}, "203.0.113.4", ClientIPForwarded, true},
		{"trusted IPv6 peer", "[::1]:1234",
			map[string][]string{"X-Real-IP": {"2001:db8::4"}}, "2001:db8::4", ClientIPForwarded, true},
		{"missing X-Real-IP", "10.0.0.2:1234", nil, "10.0.0.2", ClientIPSocket, false},
		{"repeated X-Real-IP", "10.0.0.2:1234",
			map[string][]string{"X-Real-IP": {"203.0.113.4", "198.51.100.9"}}, "10.0.0.2", ClientIPSocket, false},
		{"comma-joined X-Real-IP", "10.0.0.2:1234",
			map[string][]string{"X-Real-IP": {"203.0.113.4,198.51.100.9"}}, "10.0.0.2", ClientIPSocket, false},
		{"alongside X-Forwarded-For", "10.0.0.2:1234", map[string][]string{
			"X-Real-IP": {"203.0.113.4"}, "X-Forwarded-For": {"198.51.100.9"}}, "10.0.0.2", ClientIPSocket, false},
		{"alongside Forwarded", "10.0.0.2:1234", map[string][]string{
			"X-Real-IP": {"203.0.113.4"}, "Forwarded": {"for=198.51.100.9"}}, "10.0.0.2", ClientIPSocket, false},
		{"only X-Forwarded-For", "10.0.0.2:1234",
			map[string][]string{"X-Forwarded-For": {"203.0.113.4"}}, "10.0.0.2", ClientIPSocket, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := httptest.NewRequest("GET", "/", nil)
			r.RemoteAddr = tc.remote
			for name, values := range tc.headers {
				for _, v := range values {
					r.Header.Add(name, v)
				}
			}
			got, ok := ResolveClientAddress(r, trusted)
			if got.Addr.String() != tc.want || got.Source != tc.source || ok != tc.ok {
				t.Fatalf("ResolveClientAddress() = %s/%s/%t, want %s/%s/%t", got.Addr, got.Source, ok, tc.want,
					tc.source, tc.ok)
			}
		})
	}
}

// An IPv6 client is keyed by the /64 it controls and the /56 and /48 an allocation may hold.
func TestAddressKeysAggregateIPv6Allocations(t *testing.T) {
	for addr, want := range map[string]string{
		"203.0.113.7":                      "203.0.113.7",
		"::ffff:203.0.113.7":               "203.0.113.7",
		"2001:db8:1:2::1":                  "2001:db8:1:2::/64 2001:db8:1::/56 2001:db8:1::/48",
		"2001:db8:1:2ff:ffff:ffff:ffff:ff": "2001:db8:1:2ff::/64 2001:db8:1:200::/56 2001:db8:1::/48",
	} {
		if got := strings.Join(AddressKeys(netip.MustParseAddr(addr)), " "); got != want {
			t.Errorf("AddressKeys(%s) = %s, want %s", addr, got, want)
		}
	}
}

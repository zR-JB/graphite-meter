package transport

import (
	"net/http/httptest"
	"testing"
)

// TestClientIP covers the address-resolution rule shared by every Session
// impl: X-Forwarded-For wins when present (first hop only), else fall back to
// the socket's RemoteAddr.
func TestClientIP(t *testing.T) {
	tests := []struct {
		name       string
		xff        string
		remoteAddr string
		want       string
	}{
		{
			name:       "single XFF value",
			xff:        "203.0.113.5",
			remoteAddr: "10.0.0.1:12345",
			want:       "203.0.113.5",
		},
		{
			name:       "multi-value XFF returns first, trimmed",
			xff:        "203.0.113.5,  198.51.100.7, 10.0.0.1",
			remoteAddr: "10.0.0.1:12345",
			want:       "203.0.113.5",
		},
		{
			name:       "no XFF falls back to RemoteAddr host",
			remoteAddr: "198.51.100.9:54321",
			want:       "198.51.100.9",
		},
		{
			name:       "malformed RemoteAddr with no port returns it verbatim",
			remoteAddr: "198.51.100.9",
			want:       "198.51.100.9",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := httptest.NewRequest("GET", "/", nil)
			r.RemoteAddr = tt.remoteAddr
			if tt.xff != "" {
				r.Header.Set("X-Forwarded-For", tt.xff)
			}
			if got := ClientIP(r); got != tt.want {
				t.Errorf("ClientIP() = %q, want %q", got, tt.want)
			}
		})
	}
}

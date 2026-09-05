package transport

import (
	"net/http/httptest"
	"testing"
)

func TestHTTPProtocol(t *testing.T) {
	tests := []struct {
		protoMajor int
		want       Proto
	}{
		{1, ProtoH1},
		{2, ProtoH2},
		{3, ProtoH3},
		{0, ProtoH1}, // unrecognized major falls back to h1
	}

	for _, tt := range tests {
		r := httptest.NewRequest("GET", "/", nil)
		r.ProtoMajor = tt.protoMajor
		if got := HTTPProtocol(r); got != tt.want {
			t.Errorf("ProtoMajor=%d: Proto() = %q, want %q", tt.protoMajor, got, tt.want)
		}
	}
}

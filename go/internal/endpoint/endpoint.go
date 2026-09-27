// Package endpoint implements the measurement operations behind each route.
package endpoint

import (
	"net/http"
	"net/netip"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
)

type uploadClient struct {
	owner string
	keys  []string
}

// uploadClientOf owns by browser grant, principal or IPv6 /64; ambiguous proxy evidence owns nothing.
func uploadClientOf(r *http.Request, trusted []netip.Prefix) uploadClient {
	keys, ok := auth.ClientKeys(r, trusted)
	if !ok {
		return uploadClient{}
	}
	return uploadClient{owner: keys[0], keys: keys}
}

func noStoreJSON(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
}

// Package endpoint implements the measurement operations behind each route.
package endpoint

import (
	"net/http"
	"net/netip"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
)

// uploadClient owns receivers as a browser grant, principal or IPv6 /64, and spends that client's budgets.
type uploadClient struct {
	owner string
	keys  []string
}

// uploadClientOf is zero, owning nothing, when a trusted proxy's evidence is ambiguous.
func uploadClientOf(r *http.Request, trusted []netip.Prefix) uploadClient {
	keys, ok := auth.ClientKeys(r, trusted)
	if !ok {
		return uploadClient{}
	}
	c := uploadClient{owner: keys[0], keys: keys}
	if p, _ := auth.PrincipalFromContext(r.Context()); p.MeasurementOwner() != "" {
		c.owner = p.MeasurementOwner()
	}
	return c
}

func noStoreJSON(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
}

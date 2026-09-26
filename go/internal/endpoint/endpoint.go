// Package endpoint implements the measurement operations behind each route.
package endpoint

import (
	"context"
	"io"
	"net/http"
	"net/netip"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

type StreamFunc func(ctx context.Context, n int64, w io.Writer)

type ReceiveFunc func(ctx context.Context, id, owner string, src io.Reader) (int64, error)

// ClientKey keys upload ownership and admission by subject, IPv4 address, or IPv6 /64.
func ClientKey(r *http.Request, trusted []netip.Prefix) string {
	if p, ok := auth.PrincipalFromContext(r.Context()); ok {
		return "principal:" + p.Subject
	}
	return transport.AddressBucket(transport.ResolveClientAddress(r, trusted).Addr)
}

// UploadOwner separates delegated browser access without multiplying admission budgets.
func UploadOwner(r *http.Request, trusted []netip.Prefix) string {
	if p, ok := auth.PrincipalFromContext(r.Context()); ok && p.MeasurementOwner() != "" {
		return p.MeasurementOwner()
	}
	return ClientKey(r, trusted)
}

// SessionKey buckets the session budget by login, else by client key.
func SessionKey(r *http.Request, clientKey string) string {
	if p, ok := auth.PrincipalFromContext(r.Context()); ok && p.LoginID() != "" {
		return "login:" + p.LoginID()
	}
	return clientKey
}

func noStoreJSON(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
}
